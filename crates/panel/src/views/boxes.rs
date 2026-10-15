//! Pieces the Status page's boxes share: a box that folds to its head, as
//! the plugins' do, a read-only field with Copy, and a state with its dot.

use gloo::timers::callback::Timeout;
use yew::prelude::*;

use super::icon;

#[derive(Properties, PartialEq)]
pub struct FoldBoxProps {
    /// The box's id; its fold button is `{id}-open`, its body `{id}-body`.
    pub id: AttrValue,
    pub title: AttrValue,
    /// On the head's right, folded or not: what it says at a glance.
    #[prop_or_default]
    pub summary: Html,
    /// Opens it (at first, or when it turns true: linked to, or it needs
    /// a look).
    #[prop_or_default]
    pub open: bool,
    #[prop_or_default]
    pub children: Children,
}

/// A box folded to its head (its title, and a summary on the right) until
/// the title is clicked, like a plugin's on the Plugins page.
#[function_component]
pub fn FoldBox(props: &FoldBoxProps) -> Html {
    let open = use_state(|| props.open);
    {
        let open = open.clone();
        use_effect_with(props.open, move |wanted| {
            if *wanted {
                open.set(true);
            }
        });
    }
    let flip = {
        let open = open.clone();
        Callback::from(move |_: MouseEvent| open.set(!*open))
    };
    let id = &props.id;
    html! {
        <section class="panel-box" id={id.clone()}>
            <div class="box-head">
                <button type="button" class={classes!("box-title", "box-fold", open.then_some("open"))}
                        id={format!("{id}-open")} aria-expanded={open.to_string()} aria-controls={format!("{id}-body")}
                        onclick={flip}>
                    { icon("chevron") }
                    { props.title.clone() }
                </button>
                { props.summary.clone() }
            </div>
            if *open {
                <div class="box-body" id={format!("{id}-body")}>{ for props.children.iter() }</div>
            }
        </section>
    }
}

/// A box head's summary: a dot (`ok`, `warn`, `error`, `off`; none for
/// `""`) and a few words.
pub fn summary(dot: &str, text: impl Into<String>) -> Html {
    html! {
        <span class="plugin-status">
            if !dot.is_empty() { <span class={classes!("dot", dot.to_string())}></span> }
            { text.into() }
        </span>
    }
}

/// A state in a row: its dot and words (`id` for tests).
pub fn state_line(id: &str, dot: &str, text: impl Into<String>) -> Html {
    html! {
        <div class="state-line" id={id.to_string()}>
            <span class={classes!("dot", dot.to_string())}></span>
            <span>{ text.into() }</span>
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct CopyFieldProps {
    pub id: AttrValue,
    /// What it shows.
    pub value: AttrValue,
    /// What Copy copies, when it isn't what it shows (a hidden key).
    #[prop_or_default]
    pub copies: Option<AttrValue>,
    /// What it holds, for Copy's label: "the key".
    pub what: AttrValue,
    /// Big letters (a code to read out).
    #[prop_or_default]
    pub big: bool,
}

/// A read-only field and Copy (its button `{id}-copy`), which shows a tick
/// for a moment once it has copied.
#[function_component]
pub fn CopyField(props: &CopyFieldProps) -> Html {
    let copied = use_state(|| None::<Timeout>);
    let onclick = {
        let text = props.copies.clone().unwrap_or_else(|| props.value.clone());
        let copied = copied.clone();
        Callback::from(move |_: MouseEvent| {
            let _ = gloo::utils::window().navigator().clipboard().write_text(&text);
            let done = copied.clone();
            copied.set(Some(Timeout::new(2_000, move || done.set(None))));
        })
    };
    let id = &props.id;
    html! {
        <div class="copy-field">
            <input type="text" readonly=true id={id.clone()} class={classes!("mono", props.big.then_some("big"))}
                   value={props.value.clone()} aria-label={props.what.clone()} spellcheck="false" />
            <button type="button" class="icon-button flat" id={format!("{id}-copy")}
                    title={if copied.is_some() { "Copied" } else { "Copy" }} aria-label={format!("Copy {}", props.what)}
                    {onclick}>
                { icon(if copied.is_some() { "check" } else { "copy" }) }
            </button>
        </div>
    }
}
