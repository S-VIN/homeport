#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() -> std::io::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().unwrap_or_default();
    let port = args
        .next()
        .and_then(|s| s.to_str()?.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other("Missing lease port"))?;
    let ticket = args
        .next()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| std::io::Error::other("Missing tunnel ticket"))?;
    if mode == "--serve" {
        return rproxy_core_helper::serve(port, ticket);
    }
    #[cfg(windows)]
    if mode == "--request" {
        return rproxy_core_helper::request(port, ticket);
    }
    Err(std::io::Error::other("Unknown helper mode"))
}
