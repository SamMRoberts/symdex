import { readFileSync } from "node:fs";

export function parseConfig(path) {
    return { name: readFileSync(path, "utf8").trim() };
}

export class ConfigStore {
    load(path) {
        return parseConfig(path);
    }
}