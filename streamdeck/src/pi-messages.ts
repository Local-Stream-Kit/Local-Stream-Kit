/** What the plugin and a key's settings page say to each other. */
import type { CardState, EffectState, KindInfo } from "./protocol.js";
import type { Connection } from "./view.js";

export type ToSettingsPage = {
	type: "view";
	connection: Connection;
	/** What's wrong, for a person; empty when nothing is. */
	problem: string;
	/** Where the app is (`http://localhost:5750`), without the key. */
	base: string;
	cards: CardState[];
	effects: EffectState[];
	events: string[];
	/** What a queue's clear can be limited to. */
	kinds: KindInfo[];
};

export type FromSettingsPage =
	/** The page opened: send it the view. */
	| { type: "hello" }
	/** A new address was pasted. */
	| { type: "address"; text: string };
