/**
 * The kinds of keys. Each draws itself from the app's state
 * (`view.ts`, `keys.ts`) and, when pressed, asks the app to do something.
 */
import streamDeck, {
	action,
	type Action,
	type DidReceiveSettingsEvent,
	type KeyDownEvent,
	SingletonAction,
	type WillAppearEvent,
	type WillDisappearEvent,
} from "@elgato/streamdeck";
import type { JsonObject } from "@elgato/utils";

import type { App } from "./app.js";
import { type Icon, keyImage } from "./keys.js";
import { paths, type QueueAction } from "./protocol.js";
import {
	type CardSettings,
	type EffectSettings,
	type EventSettings,
	type HistorySettings,
	type Kind,
	type KeyView,
	keyView,
	type MuteSettings,
	type QueueSettings,
	type Settings,
} from "./view.js";

export const PLUGIN_UUID = "com.localstreamkit.streamdeck";

abstract class ControlAction<S extends Settings & JsonObject> extends SingletonAction<S> {
	abstract readonly kind: Kind;
	app!: App;
	/** What each key last showed, so a change only redraws keys it changes. */
	#shown = new Map<string, string>();

	/** The request a press makes, or nothing when the key isn't set up. */
	protected abstract path(settings: S): string | undefined;

	/** Pressing it shows a tick (for keys whose image doesn't change). */
	protected confirms(_settings: S): boolean {
		return false;
	}

	protected icon(_settings: S, _view: KeyView): Icon {
		return this.kind;
	}

	override onWillAppear(ev: WillAppearEvent<S>): Promise<void> {
		return this.draw(ev.action, ev.payload.settings);
	}

	override onWillDisappear(ev: WillDisappearEvent<S>): void {
		this.#shown.delete(ev.action.id);
	}

	override onDidReceiveSettings(ev: DidReceiveSettingsEvent<S>): Promise<void> {
		return this.draw(ev.action, ev.payload.settings);
	}

	override async onKeyDown(ev: KeyDownEvent<S>): Promise<void> {
		const settings = ev.payload.settings;
		const path = this.path(settings);
		if (!path) {
			await ev.action.showAlert();
			return;
		}
		const answer = await this.app.request(path);
		if (!answer.ok) {
			streamDeck.logger.warn(`${path}: ${answer.problem}`);
			await ev.action.showAlert();
		} else if (this.confirms(settings)) {
			await ev.action.showOk();
		}
	}

	async draw(action: Action<S>, settings: S): Promise<void> {
		if (!action.isKey()) return;
		const view = keyView(this.kind, settings, this.app.state, this.app.connection);
		const image = keyImage(this.kind, view, this.icon(settings, view));
		const shown = `${view.title}\n${image}`;
		if (this.#shown.get(action.id) === shown) return;
		this.#shown.set(action.id, shown);
		await action.setImage(image);
		await action.setTitle(view.title);
	}

	/** Redraws every key of this kind (the app's state changed). */
	async drawAll(): Promise<void> {
		for (const action of this.actions) {
			await this.draw(action, await action.getSettings());
		}
	}
}

@action({ UUID: `${PLUGIN_UUID}.card` })
export class CardAction extends ControlAction<CardSettings> {
	readonly kind = "card";

	protected path(settings: CardSettings): string | undefined {
		return settings.card ? paths.card(settings.card, settings.set ?? "toggle") : undefined;
	}
}

@action({ UUID: `${PLUGIN_UUID}.effect` })
export class EffectAction extends ControlAction<EffectSettings> {
	readonly kind = "effect";

	protected path(settings: EffectSettings): string | undefined {
		return settings.effect ? paths.effect(settings.effect, settings.set ?? "toggle") : undefined;
	}
}

@action({ UUID: `${PLUGIN_UUID}.queue` })
export class QueueKeyAction extends ControlAction<QueueSettings> {
	readonly kind = "queue";

	protected path(settings: QueueSettings): string {
		const action = settings.action ?? "toggle";
		return paths.queue(action, action === "clear" ? settings.kind : undefined);
	}

	protected override confirms(settings: QueueSettings): boolean {
		return settings.action === "skip" || settings.action === "clear";
	}

	protected override icon(settings: QueueSettings, view: KeyView): QueueAction {
		// The toggles show what a press does.
		const action = settings.action ?? "toggle";
		if (action === "toggle") return view.look === "paused" ? "resume" : "pause";
		return action;
	}
}

@action({ UUID: `${PLUGIN_UUID}.event` })
export class EventAction extends ControlAction<EventSettings> {
	readonly kind = "event";

	protected path(settings: EventSettings): string | undefined {
		return settings.event ? paths.event(settings.event) : undefined;
	}

	protected override confirms(): boolean {
		return true;
	}
}

@action({ UUID: `${PLUGIN_UUID}.history` })
export class HistoryAction extends ControlAction<HistorySettings> {
	readonly kind = "history";

	protected path(): string {
		return paths.historySeen;
	}

	protected override confirms(): boolean {
		return true;
	}
}

@action({ UUID: `${PLUGIN_UUID}.mute` })
export class MuteAction extends ControlAction<MuteSettings> {
	readonly kind = "mute";

	protected path(settings: MuteSettings): string {
		return paths.mute(settings.set ?? "toggle");
	}

	protected override icon(settings: MuteSettings, view: KeyView): Icon {
		// What a press does.
		const set = settings.set ?? "toggle";
		if (set === "toggle") return view.look === "paused" ? "unmute" : "mute";
		return set === "on" ? "mute" : "unmute";
	}
}
