import { readFileSync } from "node:fs";

export interface Config {
    name: string;
}

export function parseConfig(path: string): Config {
    const content = readFileSync(path, "utf8");
    return { name: content.trim() };
}

export class ConfigStore {
    load(path: string): Config {
        return parseConfig(path);
    }
}