import { describe, it, test } from "vitest";

export function add(left: number, right: number): number {
    return left + right;
}

const namedCase = (): void => {
    add(1, 2);
};

describe("math", () => {
    it("named case", namedCase);
    test("inline case", () => add(2, 3));
});
