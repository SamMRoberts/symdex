import { describe, it } from "mocha";

function add(left, right) {
    return left + right;
}

function namedCase() {
    add(1, 2);
}

describe("math", function () {
    it("named case", namedCase);
});
