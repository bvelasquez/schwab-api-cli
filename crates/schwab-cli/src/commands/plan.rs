use anyhow::Result;
use serde_json::json;

use crate::cli::PlanCommands;
use crate::config::RuntimeConfig;
use crate::order_status::{wait_for_order, wait_result_json, WaitCondition};
use crate::output::ResponseEnvelope;
use crate::plan::{json_schema, llm_prompt, load_plan};
use crate::safety::{execute_trading_order, require_trading_approval};

pub async fn run(runtime: &RuntimeConfig, command: PlanCommands) -> Result<()> {
    match command {
        PlanCommands::Schema => {
            runtime.emit(ResponseEnvelope::ok("plan schema", json_schema()));
        }
        PlanCommands::Prompt => {
            runtime.emit(ResponseEnvelope::ok("plan prompt", llm_prompt()));
        }
        PlanCommands::Validate { file } => {
            let plan = load_plan(&file)?;
            let api = runtime.build_api()?;
            let report = plan.validate_with_api(&runtime.safety, &api).await?;
            let mut envelope = if report.all_ok {
                ResponseEnvelope::ok("plan validate", json!(report))
            } else {
                let mut e = ResponseEnvelope::err(
                    "plan validate",
                    "One or more steps failed safety validation",
                );
                e.data = json!(report);
                e
            };
            envelope = envelope
                .with_inputs(json!({
                    "file": file.display().to_string(),
                    "plan_id": plan.plan_id,
                }))
                .with_warnings(if report.all_ok {
                    vec![]
                } else {
                    vec!["Fix failing steps or adjust safety.json before running".into()]
                })
                .with_next_actions(vec![
                    format!("schwab plan run {} --dry-run --json", file.display()),
                ]);
            runtime.emit(envelope);
        }
        PlanCommands::Show { file } => {
            let plan = load_plan(&file)?;
            runtime.emit(
                ResponseEnvelope::ok("plan show", json!(plan))
                    .with_inputs(json!({ "file": file.display().to_string() })),
            );
        }
        PlanCommands::Run {
            file,
            step,
            from_step,
        } => {
            let plan = load_plan(&file)?;
            let steps = plan.steps_filtered(step.as_deref(), from_step.as_deref())?;

            let summary = format!(
                "Execute trade plan `{}` ({} step(s): {}).",
                plan.plan_id,
                steps.len(),
                steps
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            require_trading_approval(runtime, "plan run", &summary)?;

            let api = runtime.build_api()?;
            let equity = crate::portfolio::account_equity(&api, &plan.account_hash)
                .await
                .ok()
                .flatten();

            let mut results = Vec::new();
            for (idx, step) in steps.iter().enumerate() {
                let order = plan.step_order_json(step)?;
                let wait_opts = plan.wait_options_for_step(step);

                if runtime.dry_run {
                    runtime.safety.validate_order(&order, None, equity)?;
                    results.push(json!({
                        "step_id": step.id,
                        "status": "dry_run_ok",
                        "order": order,
                        "wait_until": wait_opts.condition.as_str(),
                    }));
                    continue;
                }

                match execute_trading_order(runtime, &api, &plan.account_hash, &order).await {
                    Ok(data) => {
                        let mut step_result = json!({
                            "step_id": step.id,
                            "status": "submitted",
                            "submit": data,
                        });

                        if wait_opts.condition != WaitCondition::Accepted {
                            if let Some(order_id) = data
                                .get("order_id")
                                .and_then(|v| v.as_str())
                                .map(str::to_string)
                            {
                                match wait_for_order(&api, &plan.account_hash, &order_id, wait_opts)
                                    .await
                                {
                                    Ok(wait) => {
                                        step_result["wait"] = wait_result_json(&wait);
                                        if wait.met {
                                            step_result["status"] = json!("filled");
                                        } else {
                                            step_result["status"] = json!("wait_timeout");
                                            results.push(step_result);
                                            if plan.execution.stop_on_error {
                                                runtime.emit(
                                                    ResponseEnvelope::err(
                                                        "plan run",
                                                        wait.error.unwrap_or_else(|| {
                                                            "Order wait timed out".into()
                                                        }),
                                                    )
                                                    .with_inputs(json!({
                                                        "file": file.display().to_string(),
                                                        "plan_id": plan.plan_id,
                                                        "completed_steps": results,
                                                    })),
                                                );
                                                return Ok(());
                                            }
                                            continue;
                                        }
                                    }
                                    Err(e) => {
                                        step_result["status"] = json!("wait_failed");
                                        step_result["error"] = json!(e.to_string());
                                        results.push(step_result);
                                        if plan.execution.stop_on_error {
                                            runtime.emit(
                                                ResponseEnvelope::err("plan run", e.to_string())
                                                    .with_inputs(json!({
                                                        "file": file.display().to_string(),
                                                        "plan_id": plan.plan_id,
                                                        "completed_steps": results,
                                                    })),
                                            );
                                            return Ok(());
                                        }
                                        continue;
                                    }
                                }
                            } else {
                                step_result["status"] = json!("submitted_no_order_id");
                                step_result["warning"] = json!(
                                    "Could not parse order_id from Location; cannot wait for fill"
                                );
                            }
                        } else {
                            step_result["status"] = json!("executed");
                        }

                        results.push(step_result);
                    }
                    Err(e) => {
                        results.push(json!({
                            "step_id": step.id,
                            "status": "failed",
                            "error": e.to_string(),
                        }));
                        if plan.execution.stop_on_error {
                            runtime.emit(
                                ResponseEnvelope::err("plan run", e.to_string()).with_inputs(json!({
                                    "file": file.display().to_string(),
                                    "plan_id": plan.plan_id,
                                    "completed_steps": results,
                                })),
                            );
                            return Ok(());
                        }
                    }
                }

                if !runtime.dry_run
                    && plan.execution.pause_seconds_between_steps > 0
                    && idx + 1 < steps.len()
                {
                    tokio::time::sleep(std::time::Duration::from_secs(
                        plan.execution.pause_seconds_between_steps,
                    ))
                    .await;
                }
            }

            runtime.emit(
                ResponseEnvelope::ok(
                    "plan run",
                    json!({
                        "plan_id": plan.plan_id,
                        "dry_run": runtime.dry_run,
                        "steps": results,
                    }),
                )
                .with_inputs(json!({
                    "file": file.display().to_string(),
                    "step_filter": step,
                    "from_step": from_step,
                })),
            );
        }
    }

    Ok(())
}
