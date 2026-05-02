import { describe, it, test } from "@jest/globals";

export function add(left, right) {
    return left + right;
}

function namedCase() {
    add(1, 2);
}

describe("math", () => {
    test("named case", namedCase);
    it("inline case", () => add(2, 3));
});
