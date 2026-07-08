//! Spoken trade-event cues — embedded MP3s played via macOS `afplay`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use serde_json::Value;

static DISABLED: AtomicBool = AtomicBool::new(false);
static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeAudioEvent {
    EntryOpened,
    EntryWorking,
    EntryDeferred,
    EntryRejected,
    EntryCancelled,
    ExitProfit,
    ExitStop,
    ExitTime,
    ExitEod,
    ExitOvernight,
    ExitDte,
    ExitAdvisor,
    ExitBracket,
    AlertHalted,
    AlertRisk,
}

const ALL_EVENTS: [TradeAudioEvent; 15] = [
    TradeAudioEvent::EntryOpened,
    TradeAudioEvent::EntryWorking,
    TradeAudioEvent::EntryDeferred,
    TradeAudioEvent::EntryRejected,
    TradeAudioEvent::EntryCancelled,
    TradeAudioEvent::ExitProfit,
    TradeAudioEvent::ExitStop,
    TradeAudioEvent::ExitTime,
    TradeAudioEvent::ExitEod,
    TradeAudioEvent::ExitOvernight,
    TradeAudioEvent::ExitDte,
    TradeAudioEvent::ExitAdvisor,
    TradeAudioEvent::ExitBracket,
    TradeAudioEvent::AlertHalted,
    TradeAudioEvent::AlertRisk,
];

impl TradeAudioEvent {
    fn asset(self) -> (&'static str, &'static [u8]) {
        match self {
            Self::EntryOpened => ("entry_opened", include_bytes!("../../../../assets/trade-audio/entry_opened.mp3")),
            Self::EntryWorking => ("entry_working", include_bytes!("../../../../assets/trade-audio/entry_working.mp3")),
            Self::EntryDeferred => ("entry_deferred", include_bytes!("../../../../assets/trade-audio/entry_deferred.mp3")),
            Self::EntryRejected => ("entry_rejected", include_bytes!("../../../../assets/trade-audio/entry_rejected.mp3")),
            Self::EntryCancelled => ("entry_cancelled", include_bytes!("../../../../assets/trade-audio/entry_cancelled.mp3")),
            Self::ExitProfit => ("exit_profit", include_bytes!("../../../../assets/trade-audio/exit_profit.mp3")),
            Self::ExitStop => ("exit_stop", include_bytes!("../../../../assets/trade-audio/exit_stop.mp3")),
            Self::ExitTime => ("exit_time", include_bytes!("../../../../assets/trade-audio/exit_time.mp3")),
            Self::ExitEod => ("exit_eod", include_bytes!("../../../../assets/trade-audio/exit_eod.mp3")),
            Self::ExitOvernight => ("exit_overnight", include_bytes!("../../../../assets/trade-audio/exit_overnight.mp3")),
            Self::ExitDte => ("exit_dte", include_bytes!("../../../../assets/trade-audio/exit_dte.mp3")),
            Self::ExitAdvisor => ("exit_advisor", include_bytes!("../../../../assets/trade-audio/exit_advisor.mp3")),
            Self::ExitBracket => ("exit_bracket", include_bytes!("../../../../assets/trade-audio/exit_bracket.mp3")),
            Self::AlertHalted => ("alert_halted", include_bytes!("../../../../assets/trade-audio/alert_halted.mp3")),
            Self::AlertRisk => ("alert_risk", include_bytes!("../../../../assets/trade-audio/alert_risk.mp3")),
        }
    }
}

/// Configure audio (call once when an agent/watch session starts).
pub fn init(no_audio: bool) {
    DISABLED.store(no_audio, Ordering::Relaxed);
    if no_audio {
        return;
    }
    let _ = cache_dir();
    std::thread::spawn(|| {
        for event in ALL_EVENTS {
            let (name, bytes) = event.asset();
            let _ = ensure_cached(name, bytes);
        }
    });
}

pub fn speak(event: TradeAudioEvent) {
    if DISABLED.load(Ordering::Relaxed) {
        return;
    }
    let (name, bytes) = event.asset();
    let path = ensure_cached(name, bytes);
    play_path(&path);
}

pub fn speak_exit_reason(reason: &str) {
    let event = match reason {
        "profit_target" => TradeAudioEvent::ExitProfit,
        "stop_loss" => TradeAudioEvent::ExitStop,
        "time_stop" => TradeAudioEvent::ExitTime,
        "eod_flatten" => TradeAudioEvent::ExitEod,
        "overnight_flatten" => TradeAudioEvent::ExitOvernight,
        "dte_close" => TradeAudioEvent::ExitDte,
        "llm_recommendation" => TradeAudioEvent::ExitAdvisor,
        "oco_filled" => TradeAudioEvent::ExitBracket,
        _ => return,
    };
    speak(event);
}

/// Map options-agent Telegram action payloads to spoken cues.
pub fn speak_from_action(kind: &str, detail: &Value) {
    if let Some(fill) = detail.get("fill_status").and_then(|v| v.as_str()) {
        speak_from_order_status(kind, fill, detail);
        return;
    }
    if detail.get("exit").is_some() || kind.contains("EXIT") {
        let reason = detail
            .pointer("/signal/reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        speak_exit_reason(reason);
    }
}

fn speak_from_order_status(kind: &str, fill_status: &str, detail: &Value) {
    if fill_status.eq_ignore_ascii_case("SKIPPED") {
        return;
    }
    let upper = fill_status.to_ascii_uppercase();
    match upper.as_str() {
        "FILLED" if kind.contains("ENTRY") => speak(TradeAudioEvent::EntryOpened),
        "FILLED" if kind.contains("EXIT") => {
            let reason = detail
                .pointer("/signal/reason")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            speak_exit_reason(reason);
        }
        "WORKING" | "ACCEPTED" | "PENDING_ACTIVATION" | "QUEUED" => {
            speak(TradeAudioEvent::EntryWorking);
        }
        "REJECTED" | "CANCELED" | "CANCELLED" | "EXPIRED" => speak(TradeAudioEvent::EntryRejected),
        _ if kind.contains("REJECTED") => speak(TradeAudioEvent::EntryRejected),
        _ => {}
    }
}

fn cache_dir() -> PathBuf {
    CACHE_DIR
        .get_or_init(|| {
            let dir = std::env::temp_dir().join("schwab-trade-audio");
            let _ = std::fs::create_dir_all(&dir);
            dir
        })
        .clone()
}

fn ensure_cached(name: &str, bytes: &[u8]) -> PathBuf {
    let path = cache_dir().join(format!("{name}.mp3"));
    let needs_write = match std::fs::metadata(&path) {
        Ok(meta) => meta.len() != bytes.len() as u64,
        Err(_) => true,
    };
    if needs_write {
        let _ = std::fs::write(&path, bytes);
    }
    path
}

fn play_path(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("afplay")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        tracing::debug!(target: "trade_audio", "trade audio playback is macOS-only (afplay)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_reason_maps_to_events() {
        assert_eq!(
            exit_reason_event("profit_target"),
            Some(TradeAudioEvent::ExitProfit)
        );
        assert_eq!(exit_reason_event("dte_close"), Some(TradeAudioEvent::ExitDte));
        assert_eq!(exit_reason_event("unknown"), None);
    }

    fn exit_reason_event(reason: &str) -> Option<TradeAudioEvent> {
        match reason {
            "profit_target" => Some(TradeAudioEvent::ExitProfit),
            "stop_loss" => Some(TradeAudioEvent::ExitStop),
            "time_stop" => Some(TradeAudioEvent::ExitTime),
            "eod_flatten" => Some(TradeAudioEvent::ExitEod),
            "overnight_flatten" => Some(TradeAudioEvent::ExitOvernight),
            "dte_close" => Some(TradeAudioEvent::ExitDte),
            "llm_recommendation" => Some(TradeAudioEvent::ExitAdvisor),
            "oco_filled" => Some(TradeAudioEvent::ExitBracket),
            _ => None,
        }
    }
}
