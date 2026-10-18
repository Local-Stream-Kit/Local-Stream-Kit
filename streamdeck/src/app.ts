/**
 * The connection to Local Stream Kit: its state over the control API's
 * WebSocket (sent on connecting and on every change), and presses as web
 * requests, so each gets its own answer. On connecting, the plugin says
 * its version, for the app's Status.
 */
import { type Address, parseAddress, webSocketUrl } from "./address.js";
import { CONTROL_VERSION, type ControlMessage, type ControlState, hello, KEY_HEADER, paths } from "./protocol.js";
import type { Connection } from "./view.js";

/** Waits before trying again, growing to the last. */
const RETRY_MS = [1_000, 2_000, 5_000, 10_000];

export type Answer = { ok: true } | { ok: false; problem: string };

export class App {
	#address: Address | undefined;
	#socket: WebSocket | undefined;
	#retry: ReturnType<typeof setTimeout> | undefined;
	#attempt = 0;
	/** Bumped on every new address, so an old socket's events are ignored. */
	#generation = 0;
	#listeners: (() => void)[] = [];

	/** Connecting until the saved address is read (`setAddress`). */
	connection: Connection = "connecting";
	/** The latest state, while connected. */
	state: ControlState | undefined;
	/** Why the connection is what it is, for the settings page. */
	problem = "";

	/** `version`: the plugin's, as its manifest says (`0.2.0.0`). */
	constructor(readonly version: string) {}

	/** Called whenever the connection or the state changes. */
	onChange(listener: () => void): void {
		this.#listeners.push(listener);
	}

	/** Where the app is, from the pasted address (the plugin's global setting). */
	get base(): string | undefined {
		return this.#address?.base;
	}

	/** Connects to `text` (as pasted), dropping any earlier connection. */
	setAddress(text: string | undefined): void {
		this.#generation++;
		clearTimeout(this.#retry);
		this.#socket?.close();
		this.#socket = undefined;
		this.#attempt = 0;
		this.state = undefined;
		const parsed = parseAddress(text ?? "");
		if ("problem" in parsed) {
			this.#address = undefined;
			this.#set("unset", text?.trim() ? parsed.problem : "");
			return;
		}
		this.#address = parsed;
		this.#connect();
	}

	#set(connection: Connection, problem: string): void {
		this.connection = connection;
		this.problem = problem;
		for (const listener of this.#listeners) {
			listener();
		}
	}

	#connect(): void {
		const address = this.#address;
		if (!address) return;
		const generation = this.#generation;
		const current = () => generation === this.#generation;
		this.#set("connecting", "");
		let opened = false;
		const socket = new WebSocket(webSocketUrl(address, paths.ws));
		this.#socket = socket;
		socket.addEventListener("open", () => {
			if (!current()) return;
			opened = true;
			this.#attempt = 0;
			socket.send(hello(this.version));
		});
		socket.addEventListener("message", (event) => {
			if (!current() || typeof event.data !== "string") return;
			let message: ControlMessage;
			try {
				message = JSON.parse(event.data);
			} catch {
				return;
			}
			if (message.type !== "state") return;
			if (message.state.version > CONTROL_VERSION) {
				this.state = undefined;
				this.#set("too-new", "Local Stream Kit is newer than this plugin: update the plugin.");
				socket.close();
				return;
			}
			if (message.state.version < CONTROL_VERSION) {
				this.state = undefined;
				this.#set("too-old", "Local Stream Kit is older than this plugin: update Local Stream Kit.");
				socket.close();
				return;
			}
			this.state = message.state;
			this.#set("connected", "");
		});
		socket.addEventListener("close", () => {
			if (!current()) return;
			this.state = undefined;
			if (this.connection === "too-new" || this.connection === "too-old") return;
			if (opened) {
				this.#set("connecting", "Local Stream Kit went away; trying again.");
				this.#later();
			} else {
				void this.#whyNot(address, generation);
			}
		});
	}

	/** The WebSocket didn't open: a plain request tells a wrong key from no app. */
	async #whyNot(address: Address, generation: number): Promise<void> {
		let status = 0;
		try {
			const response = await fetch(new URL(paths.state, address.base), { headers: { [KEY_HEADER]: address.key } });
			status = response.status;
		} catch {
			// Nothing answers.
		}
		if (generation !== this.#generation) return;
		if (status === 401) {
			this.#set("wrong-key", "Wrong key: copy the address again from Local Stream Kit's Control API page.");
		} else {
			this.#set("unreachable", `Local Stream Kit doesn't answer at ${address.base}. Is it running?`);
		}
		this.#later();
	}

	#later(): void {
		const wait = RETRY_MS[Math.min(this.#attempt, RETRY_MS.length - 1)];
		this.#attempt++;
		clearTimeout(this.#retry);
		this.#retry = setTimeout(() => this.#connect(), wait);
	}

	/** A key was pressed: asks the app to do `path` (POST), and says how it went. */
	async request(path: string): Promise<Answer> {
		const address = this.#address;
		if (!address) {
			return { ok: false, problem: "Not set up: paste the address in the key's settings." };
		}
		try {
			const response = await fetch(new URL(path, address.base), {
				method: "POST",
				headers: { [KEY_HEADER]: address.key },
			});
			if (response.ok) {
				return { ok: true };
			}
			const body = (await response.json().catch(() => ({}))) as { error?: string };
			return { ok: false, problem: body.error ?? `Local Stream Kit answered ${response.status}` };
		} catch {
			return { ok: false, problem: `Local Stream Kit doesn't answer at ${address.base}.` };
		}
	}
}
