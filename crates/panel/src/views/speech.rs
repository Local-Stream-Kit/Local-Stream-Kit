//! Text to speech, on the Speak plugin's box (whose switch is every voice
//! read aloud): installing Kokoro, the default voice, speed and volume, Try,
//! and the reading rules.

use protocol::panel::PanelRequest;
use protocol::plugin::{plugin_voice, split_voice};
use protocol::speech::{Accent, Pronunciation, SpeechSettings, SpeechStatus, TRY_TEXT, VOICES, Voice};
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::editor::fields::{NumberField, TextField};
use crate::views::icon;
use crate::{Panel, util};

/// The voices as options: Kokoro's, British first, labelled with accent
/// and grade, under headers that say they're Kokoro's; then each plugin's
/// that's on, under its name.
pub fn voice_options(panel: &Panel, selected: &str) -> Html {
    let plugins: Vec<(String, Vec<(String, String)>)> = panel
        .model
        .plugins
        .plugins()
        .iter()
        .filter(|plugin| !plugin.manifest.voices.is_empty())
        .filter(|plugin| {
            panel.model.config.as_ref().is_none_or(|config| config.plugin(&plugin.manifest.id).enabled)
        })
        .map(|plugin| {
            let voices = plugin
                .manifest
                .voices
                .iter()
                .map(|voice| (plugin_voice(&plugin.manifest.id, &voice.id), voice.name.clone()))
                .collect();
            (plugin.manifest.name.clone(), voices)
        })
        .collect();
    let known = Voice::find(selected).is_some()
        || plugins.iter().any(|(_, voices)| voices.iter().any(|(id, _)| id == selected));
    let group = |accent: Accent, label: &'static str| {
        html! {
            <optgroup {label}>
                { for VOICES.iter().filter(|voice| voice.accent == accent).map(|voice| html! {
                    <option value={voice.id} selected={voice.id == selected}>{ voice.label() }</option>
                }) }
            </optgroup>
        }
    };
    html! {
        <>
            { group(Accent::British, "Kokoro · British") }
            { group(Accent::American, "Kokoro · American") }
            { for plugins.into_iter().map(|(name, voices)| html! {
                <optgroup label={name}>
                    { for voices.into_iter().map(|(id, label)| html! {
                        <option selected={id == selected} value={id.clone()}>{ label }</option>
                    }) }
                </optgroup>
            }) }
            if !known && !selected.is_empty() {
                <option value={selected.to_string()} selected=true>{ selected.to_string() }</option>
            }
        </>
    }
}

fn edit(panel: &Panel, field: &str, change: impl Fn(&mut SpeechSettings) + 'static) {
    let key = format!("speech/{field}");
    panel.edit(Some(&key), move |config| change(&mut config.effects.speech.settings));
}

/// A settings row as the plugin boxes have them: the label, then the
/// control. `wraps`: its words may take two lines (in the dock), the label
/// and a tick box staying on the first.
fn row(id: &str, label: &str, wraps: bool, control: Html) -> Html {
    html! {
        <div class={classes!("setting-row", wraps.then_some("wraps"))}>
            <label class="setting-label" for={id.to_string()}>{ label.to_string() }</label>
            <div class="setting-control">{ control }</div>
        </div>
    }
}

/// A tick box with its words after it.
fn tick(id: &str, checked: bool, words: &str, onchange: Callback<Event>) -> Html {
    html! {
        <div class="check-field wraps">
            <input type="checkbox" id={id.to_string()} {checked} {onchange} />
            <label for={id.to_string()}>{ words.to_string() }</label>
        </div>
    }
}

#[function_component]
pub fn Speech() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let try_field = use_node_ref();
    let Some(config) = &panel.model.config else { return html! {} };
    let speech = &config.effects.speech;
    let settings = &speech.settings;
    let status = &panel.model.status.speech;
    let reading = panel.model.speech_reading;
    // A plugin's voice reads without Kokoro.
    let can_read = {
        let installed = *status == SpeechStatus::Ready;
        move |voice: &str| installed || split_voice(voice).is_some()
    };

    // Reads the Try text as it is in its field (typed, maybe not saved
    // yet) on the overlay, with these settings.
    let read = {
        let (panel, try_field) = (panel.clone(), try_field.clone());
        move |settings: SpeechSettings| {
            let typed = try_field.cast::<HtmlInputElement>().map(|field| field.value()).unwrap_or_default();
            let text = if typed.trim().is_empty() { TRY_TEXT.to_string() } else { typed };
            panel.send(PanelRequest::PreviewSpeech { text, settings });
        }
    };
    let install = {
        let panel = panel.clone();
        Callback::from(move |_| {
            panel.send(PanelRequest::InstallSpeech);
        })
    };
    // Picking a voice reads Try with it.
    let on_voice = {
        let (panel, settings, read) = (panel.clone(), settings.clone(), read.clone());
        Callback::from(move |event: Event| {
            let voice = util::value_of(&event);
            let picked = voice.clone();
            edit(&panel, "voice", move |settings| settings.voice = picked.clone());
            if can_read(&voice) {
                read(SpeechSettings { voice, ..settings.clone() });
            }
        })
    };
    let slider = |field: &'static str,
                  label: &str,
                  value: f32,
                  (min, max, step): (f32, f32, f32),
                  show: String,
                  set: fn(&mut SpeechSettings, f32)| {
        let panel = panel.clone();
        let oninput = Callback::from(move |event: InputEvent| {
            if let Some(number) = util::number_of(&event) {
                edit(&panel, field, move |settings| set(settings, number as f32));
            }
        });
        let id = format!("speech-{field}");
        row(
            &id,
            label,
            false,
            html! {
                <div class="slider-row">
                    <input type="range" id={id.clone()} min={min.to_string()} max={max.to_string()}
                           step={step.to_string()} value={value.to_string()} {oninput} />
                    <span class="slider-value mono">{ show }</span>
                </div>
            },
        )
    };
    // Saved when the field is left; emptied, it stays empty (Try reads
    // its placeholder, the default).
    let on_try = {
        let panel = panel.clone();
        Callback::from(move |typed: String| {
            let text = typed.trim().to_string();
            panel.edit(None, move |config| config.effects.speech.try_text = Some(text.clone()));
        })
    };
    let say = {
        let (settings, read) = (settings.clone(), read.clone());
        Callback::from(move |_| read(settings.clone()))
    };
    let on_max_chars = {
        let panel = panel.clone();
        Callback::from(move |number: f64| {
            edit(&panel, "max_chars", move |settings| {
                settings.max_chars = number.clamp(0.0, 100_000.0) as u32
            });
        })
    };
    let on_repeats = {
        let panel = panel.clone();
        Callback::from(move |event: Event| {
            let on = util::checked_of(&event);
            edit(&panel, "collapse_repeats", move |settings| settings.collapse_repeats = on);
        })
    };
    let on_links = {
        let panel = panel.clone();
        Callback::from(move |event: Event| {
            let on = util::checked_of(&event);
            edit(&panel, "skip_links", move |settings| settings.skip_links = on);
        })
    };
    let add_word = {
        let panel = panel.clone();
        Callback::from(move |_| {
            panel.edit(None, |config| {
                config
                    .effects
                    .speech
                    .settings
                    .pronunciations
                    .push(Pronunciation { word: String::new(), say: String::new() })
            });
        })
    };

    // Kokoro, until it's installed: download it, how far it's got, or why
    // it failed.
    let kokoro = match status {
        SpeechStatus::Ready => None,
        SpeechStatus::NotInstalled => Some(html! {
            <div class="speech-install">
                <button type="button" class="button primary small" id="speech-install" onclick={install}
                        title="Kokoro: the voice model, its English voices, a model for pronouncing unknown words, and ONNX Runtime">
                    { icon("download") }{ "Download (460 MB)" }
                </button>
                <span class="small muted">{ "Runs on this PC, offline" }</span>
            </div>
        }),
        SpeechStatus::Installing { step, percent } => Some(html! {
            <div class="speech-install" role="status">
                <div class="progress"><div class="progress-bar" style={format!("width: {percent}%")}></div></div>
                <span class="small muted">{ format!("{step}… {percent}%") }</span>
            </div>
        }),
        SpeechStatus::Failed { error } => Some(html! {
            <div class="speech-install">
                <p class="error small" role="alert">{ error.clone() }</p>
                <button type="button" class="button ghost small" id="speech-install" onclick={install}>{ "Try again" }</button>
            </div>
        }),
    };

    html! {
        <div class="plugin-settings" id="speech-app">
            <div class="setting-rows">
                if let Some(kokoro) = kokoro {
                    <div class="setting-row tall wraps">
                        <span class="setting-label">{ "Kokoro" }</span>
                        <div class="setting-control">{ kokoro }</div>
                    </div>
                }
                // A spinner after "Voice" while Try is being read (from a
                // voice picked, or Say it), until it has been: beside the
                // label, so the picker doesn't move.
                <div class="setting-row">
                    <label class="setting-label" for="speech-voice">
                        { "Voice" }
                        if reading {
                            <span class="spinner" id="speech-reading" role="status"
                                  aria-label="Reading the Try text" title="Reading the Try text"></span>
                        }
                    </label>
                    <div class="setting-control">
                        <select id="speech-voice" onchange={on_voice} title="Picking a voice reads the Try text with it">
                            { voice_options(&panel, &settings.voice) }
                        </select>
                    </div>
                </div>
                { slider("speed", "Speed", settings.speed, (0.5, 2.0, 0.05), format!("{:.2}×", settings.speed), |s, v| s.speed = v) }
                { slider("volume", "Volume", settings.volume, (0.0, 1.0, 0.05), format!("{:.0}%", settings.volume * 100.0), |s, v| s.volume = v) }
                { row("speech-sample", "Try", false, html! {
                    <div class="speech-try">
                        <TextField id="speech-sample" field={try_field} value={speech.try_text().to_string()}
                                   placeholder={TRY_TEXT} onchange={on_try} />
                        <button type="button" class="button ghost small" id="speech-say" disabled={!can_read(&settings.voice)}
                                onclick={say} title="Reads it on the overlay with these settings">
                            { icon("play") }{ "Say it" }
                        </button>
                    </div>
                }) }
            </div>
            <div class="setting-group">
                <h3>{ "Reading" }</h3>
                <div class="setting-rows">
                    { row("speech-max-chars", "Read at most", false, html! {
                        <div class="number-row">
                            <NumberField id="speech-max-chars" value={f64::from(settings.max_chars)} onchange={on_max_chars} />
                            <span class="number-caption">{ "characters (0: all)" }</span>
                        </div>
                    }) }
                    { row("speech-repeats", "Repeats", true,
                          tick("speech-repeats", settings.collapse_repeats, "Read once: “KEKW KEKW KEKW” → “KEKW”", on_repeats)) }
                    { row("speech-links", "Links", true, tick("speech-links", settings.skip_links, "Leave out", on_links)) }
                    <div class="setting-row tall list">
                        <span class="setting-label" id="speech-say-label">{ "Say instead" }</span>
                        <div class="setting-control">
                            <div class="list-rows" role="group" aria-labelledby="speech-say-label">
                                { for settings.pronunciations.iter().enumerate().map(|(n, entry)| {
                                    let set = |field: &'static str, apply: fn(&mut Pronunciation, String)| {
                                        let panel = panel.clone();
                                        Callback::from(move |event: InputEvent| {
                                            let value = util::value_of(&event);
                                            edit(&panel, &format!("say-{n}-{field}"), move |settings| {
                                                if let Some(entry) = settings.pronunciations.get_mut(n) {
                                                    apply(entry, value.clone());
                                                }
                                            });
                                        })
                                    };
                                    let remove = {
                                        let panel = panel.clone();
                                        Callback::from(move |_| {
                                            panel.edit(None, move |config| {
                                                if n < config.effects.speech.settings.pronunciations.len() {
                                                    config.effects.speech.settings.pronunciations.remove(n);
                                                }
                                            });
                                        })
                                    };
                                    html! {
                                        <div class="list-row say-row">
                                            <input type="text" aria-label="Word" placeholder="Word" spellcheck="false"
                                                   value={entry.word.clone()} oninput={set("word", |e, v| e.word = v)} />
                                            <span class="muted">{ "→" }</span>
                                            <input type="text" aria-label="Say instead" placeholder="Say" spellcheck="false"
                                                   value={entry.say.clone()} oninput={set("say", |e, v| e.say = v)} />
                                            <button type="button" class="tier-remove" aria-label="Remove" title="Take it out"
                                                    onclick={remove}>{ "×" }</button>
                                        </div>
                                    }
                                }) }
                                <div>
                                    <button type="button" class="button ghost small" id="speech-add-word" onclick={add_word}>
                                        { icon("plus") }{ "Word" }
                                    </button>
                                </div>
                            </div>
                        </div>
                    </div>
                </div>
            </div>
        </div>
    }
}
