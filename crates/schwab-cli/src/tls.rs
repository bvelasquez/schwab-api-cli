/// Install rustls ring backend once per process (required for HTTPS OAuth callback).
pub fn install_crypto_provider() {
    use rustls::crypto::CryptoProvider;
    let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());
}
