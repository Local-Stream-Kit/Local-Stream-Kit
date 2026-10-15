/**
 * The app's address, as pasted from Local Stream Kit's Control API page:
 * `http://localhost:5750/control/?key=…`. It holds the key, so it's the
 * only thing the plugin needs to be told.
 */
export type Address = {
	/** `http://localhost:5750` */
	base: string;
	key: string;
};

/** The address, or what's wrong with what was pasted. */
export function parseAddress(text: string): Address | { problem: string } {
	let trimmed = text.trim();
	if (trimmed === "") {
		return { problem: "Paste the address from Local Stream Kit's Control API page." };
	}
	if (!/^[a-z]+:\/\//i.test(trimmed)) {
		trimmed = `http://${trimmed}`;
	}
	let url: URL;
	try {
		url = new URL(trimmed);
	} catch {
		return { problem: "That isn't an address. Copy it from Local Stream Kit's Control API page." };
	}
	if (url.protocol !== "http:" && url.protocol !== "https:") {
		return { problem: "The address starts with http://." };
	}
	const key = url.searchParams.get("key")?.trim() ?? "";
	if (key === "" || key === "…") {
		return { problem: "The address has no key: copy it with the copy button on the Control API page." };
	}
	return { base: url.origin, key };
}

/** The WebSocket's URL (`ws://…/control/ws?key=…`). */
export function webSocketUrl(address: Address, path: string): string {
	const url = new URL(path, address.base);
	url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
	url.searchParams.set("key", address.key);
	return url.href;
}
