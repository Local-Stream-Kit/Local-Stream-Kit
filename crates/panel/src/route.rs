//! Pages are addressed by the URL's hash (`#/cards`, `#/card/<id>`, …), so
//! they work as links, survive a reload and need no server routes.

use gloo::events::EventListener;
use yew::prelude::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Cards,
    Activity,
    Card(String),
    NewCard,
    Test,
    Status,
    Login,
    Control,
    /// The plugins, each with its effect's settings.
    Plugins,
    /// The plugins, scrolled to this one's box.
    Plugin(String),
}

impl Route {
    pub fn parse(hash: &str) -> Route {
        let path = hash.trim_start_matches('#').trim_start_matches('/');
        match path.split_once('/') {
            Some(("card", id)) if !id.is_empty() => Route::Card(decode(id)),
            Some(("plugins", id)) if !id.is_empty() => Route::Plugin(decode(id)),
            _ => match path {
                "new" => Route::NewCard,
                "activity" => Route::Activity,
                // The Effects page, until 2026-10-12.
                "effects" => Route::Plugins,
                "test" => Route::Test,
                "status" => Route::Status,
                "login" => Route::Login,
                "control" => Route::Control,
                "plugins" => Route::Plugins,
                _ => Route::Cards,
            },
        }
    }

    pub fn href(&self) -> String {
        match self {
            Route::Cards => "#/cards".into(),
            Route::Activity => "#/activity".into(),
            Route::Card(id) => format!("#/card/{}", encode(id)),
            Route::NewCard => "#/new".into(),
            Route::Test => "#/test".into(),
            Route::Status => "#/status".into(),
            Route::Login => "#/login".into(),
            Route::Control => "#/control".into(),
            Route::Plugins => "#/plugins".into(),
            Route::Plugin(id) => format!("#/plugins/{}", encode(id)),
        }
    }

    pub fn go(&self) {
        let _ = gloo::utils::window().location().set_hash(&self.href());
    }
}

fn encode(id: &str) -> String {
    js_sys::encode_uri_component(id).into()
}

fn decode(id: &str) -> String {
    js_sys::decode_uri_component(id).map(String::from).unwrap_or_else(|_| id.to_string())
}

fn current() -> Route {
    Route::parse(&gloo::utils::window().location().hash().unwrap_or_default())
}

/// The current page, following the URL.
#[hook]
pub fn use_route() -> Route {
    let route = use_state(current);
    {
        let route = route.clone();
        use_effect_with((), move |_| {
            let listener =
                EventListener::new(&gloo::utils::window(), "hashchange", move |_| route.set(current()));
            move || drop(listener)
        });
    }
    (*route).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        assert_eq!(Route::parse(""), Route::Cards);
        assert_eq!(Route::parse("#/effects"), Route::Plugins);
        assert_eq!(Route::parse("#/card/"), Route::Cards);
    }
}
