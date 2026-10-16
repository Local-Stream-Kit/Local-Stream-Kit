/**
 * The Stream Deck plugin for Local Stream Kit's control API: keys that switch
 * cards and effects, run the queue, fire Custom events and mark the history
 * seen, and show what's on. The app's address (with its key) is pasted once, in any key's
 * settings, and kept as the plugin's global setting.
 */
import streamDeck from "@elgato/streamdeck";

import { CardAction, EffectAction, EventAction, HistoryAction, MuteAction, QueueKeyAction } from "./actions.js";
import { App } from "./app.js";
import { parseAddress } from "./address.js";
import type { FromSettingsPage, ToSettingsPage } from "./pi-messages.js";

type GlobalSettings = { address?: string };

// The version Stream Deck read from the manifest.
const app = new App(streamDeck.info.plugin.version);
const actions = [
	new CardAction(),
	new EffectAction(),
	new QueueKeyAction(),
	new MuteAction(),
	new EventAction(),
	new HistoryAction(),
];
for (const action of actions) {
	action.app = app;
	streamDeck.actions.registerAction(action);
}

/** What the open settings page shows: the connection and what there is to pick. */
function pageView(): ToSettingsPage {
	return {
		type: "view",
		connection: app.connection,
		problem: app.problem,
		base: app.base ?? "",
		cards: app.state?.cards ?? [],
		effects: app.state?.effects ?? [],
		events: app.state?.events ?? [],
		kinds: app.state?.kinds ?? [],
	};
}

async function tellSettingsPage(): Promise<void> {
	if (streamDeck.ui.action) {
		await streamDeck.ui.sendToPropertyInspector(pageView());
	}
}

app.onChange(() => {
	for (const action of actions) {
		void action.drawAll();
	}
	void tellSettingsPage();
});

streamDeck.ui.onSendToPlugin<FromSettingsPage>(async (ev) => {
	const message = ev.payload;
	if (message.type === "hello") {
		await tellSettingsPage();
	} else if (message.type === "address") {
		const parsed = parseAddress(message.text);
		if ("problem" in parsed) {
			await streamDeck.ui.sendToPropertyInspector({ ...pageView(), problem: parsed.problem });
			return;
		}
		await streamDeck.settings.setGlobalSettings<GlobalSettings>({ address: message.text.trim() });
		app.setAddress(message.text);
	}
});

streamDeck.settings.onDidReceiveGlobalSettings<GlobalSettings>((ev) => app.setAddress(ev.settings.address));

await streamDeck.connect();
app.setAddress((await streamDeck.settings.getGlobalSettings<GlobalSettings>()).address);
