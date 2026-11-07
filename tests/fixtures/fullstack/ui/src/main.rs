//! dioxus web frontend of the full-stack fixture.
//!
//! Renders one page that fetches `GET /api/v1/greeting` and shows the message
//! inside the element with id [`GREETING_ID`], so a browser test can assert on
//! a stable selector.

use dioxus::prelude::*;
use shared::Greeting;

/// DOM id of the element that carries the greeting text.
pub const GREETING_ID: &str = "greeting";
/// Text shown while the request is in flight.
pub const LOADING_TEXT: &str = "loading";
pub const BUMP_ID: &str = "bump";
pub const CLICKS_ID: &str = "clicks";
pub const NAME_ID: &str = "name";
pub const ECHO_ID: &str = "echo";
/// Path the page fetches; relative so the api can serve the page.
pub const GREETING_PATH: &str = "/api/v1/greeting";

fn main() {
    dioxus::launch(App);
}

/// Maps the state of the greeting request to the text rendered in the page.
pub fn greeting_text(state: Option<&Result<Greeting, String>>) -> String {
    match state {
        None => LOADING_TEXT.to_string(),
        Some(Ok(greeting)) => greeting.message.clone(),
        Some(Err(err)) => format!("error: {err}"),
    }
}

pub fn clicks_text(count: u32) -> String {
    format!("clicks: {count}")
}

pub fn echo_text(name: &str) -> String {
    format!("typed: [{name}]")
}

async fn fetch_greeting() -> Result<Greeting, String> {
    let response = gloo_net::http::Request::get(GREETING_PATH)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.ok() {
        return Err(format!("status {}", response.status()));
    }
    response.json::<Greeting>().await.map_err(|e| e.to_string())
}

#[component]
fn App() -> Element {
    let greeting = use_resource(fetch_greeting);
    let text = greeting_text(greeting.read().as_ref());
    let mut count = use_signal(|| 0u32);
    let mut name = use_signal(String::new);
    let clicks = clicks_text(count());
    let echo = echo_text(&name.read());
    rsx! {
        main {
            h1 { "Full-stack fixture" }
            p { id: GREETING_ID, "{text}" }
            button { id: BUMP_ID, onclick: move |_| count += 1, "bump" }
            p { id: CLICKS_ID, "{clicks}" }
            textarea { id: NAME_ID, oninput: move |event| name.set(event.value()) }
            p { id: ECHO_ID, "{echo}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_state_renders_placeholder() {
        assert_eq!(greeting_text(None), LOADING_TEXT);
    }

    #[test]
    fn success_state_renders_message() {
        let ok = Ok(Greeting::default());
        assert_eq!(greeting_text(Some(&ok)), shared::DEFAULT_GREETING);
    }

    #[test]
    fn clicks_and_echo_render_their_state() {
        assert_eq!(clicks_text(0), "clicks: 0");
        assert_eq!(clicks_text(2), "clicks: 2");
        assert_eq!(echo_text(""), "typed: []");
        assert_eq!(echo_text("abc"), "typed: [abc]");
    }

    #[test]
    fn error_state_renders_error() {
        let err = Err("status 500".to_string());
        assert_eq!(greeting_text(Some(&err)), "error: status 500");
    }
}
