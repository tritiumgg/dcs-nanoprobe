//! `hello-campaign`: run the hello campaign against a server.
//!
//! The server's URL is the first argument, or `NANOPROBE_ADDR`, or
//! `http://127.0.0.1:47311`. A token, if the server needs one, is read from
//! `NANOPROBE_TOKEN`.

use std::process::ExitCode;
use std::time::Duration;

#[tokio::main]
async fn main() -> ExitCode {
    let endpoint = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("NANOPROBE_ADDR").ok())
        .unwrap_or_else(|| "http://127.0.0.1:47311".to_owned());
    let token = std::env::var("NANOPROBE_TOKEN").ok();
    match nanoprobe_hello::run(&endpoint, token.as_deref(), Duration::from_secs(120)).await {
        Ok(s) => {
            println!(
                "hello campaign done: campaign {} group {} revision {} events {}",
                s.campaign, s.group, s.revision, s.events
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("hello-campaign: {e}");
            ExitCode::FAILURE
        }
    }
}
