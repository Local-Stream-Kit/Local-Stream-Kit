//! Import, on the New card page: a card file ([`CardFile`], what the card
//! editor's Export saves) read here in the panel and added at the end, as
//! new cards are, on (as a new card from a preset), whose editor then
//! opens. And the problem line both pages show when Export or Import
//! didn't work.

use gloo::file::callbacks::FileReader;
use protocol::cards::{CardFile, CardFileError};
use yew::prelude::*;

use crate::route::Route;
use crate::{Panel, util};

/// Reads `file` and adds its card at the end, as one undo step, or says
/// why not. Dropping the reader stops the reading.
pub fn import(panel: &Panel, file: web_sys::File) -> FileReader {
    panel.card_file_problem(None);
    let name = file.name();
    let panel = panel.clone();
    let read = move |text: Result<String, _>| {
        let parsed = text.map_err(|_| CardFileError::NotACard).and_then(|text| CardFile::parse(&text));
        let mut card = match parsed {
            Ok(card) => card,
            Err(error) => {
                panel.card_file_problem(Some(match error {
                    CardFileError::NotACard => format!("Not imported: “{name}” isn't a card."),
                    CardFileError::Newer => format!(
                        "Not imported: “{name}” is from a newer version. Update Local Stream Kit (Status), then try again."
                    ),
                }));
                return;
            }
        };
        let existing = panel.model.config.as_ref().map(|config| config.cards.as_slice()).unwrap_or_default();
        card.attach_plugins(&panel.model.plugins);
        card.id = util::new_card_id(&card.name, existing);
        card.enabled = true;
        let new_id = card.id.clone();
        // Not `add_card`: an imported card is never taken out again for
        // being left unchanged, as an empty one is.
        panel.edit(None, move |config| config.cards.push(card.clone()));
        Route::Card(new_id).go();
    };
    gloo::file::callbacks::read_as_text(&gloo::file::File::from(file), read)
}

/// Keeps a page from showing another page's (or card's) export or import
/// problem: it goes when `page` changes, and when the page opens.
#[hook]
pub fn use_own_problem(page: String) {
    let panel = use_context::<Panel>().expect("panel context");
    use_effect_with(page, move |_| {
        if panel.model.card_file_problem.is_some() {
            panel.card_file_problem(None);
        }
    });
}
