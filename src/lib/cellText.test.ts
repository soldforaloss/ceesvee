import { describe, expect, it } from "vitest";

import { containsNul, countLines, escapeCellText, hasInvisibles, utf8ByteLength } from "./cellText";

// Specials are constructed, never written literally, so this file contains no
// invisible characters.
const NUL = String.fromCharCode(0);
const BEL = String.fromCharCode(7);
const TAB = String.fromCharCode(9);
const LF = String.fromCharCode(10);
const CR = String.fromCharCode(13);
const NBSP = String.fromCharCode(0xa0);
const ZWSP = String.fromCharCode(0x200b);
const REPLACEMENT = String.fromCharCode(0xfffd);
const BACKSLASH = String.fromCharCode(92);

describe("cell text inspection (F13)", () => {
  it("counts UTF-8 bytes, not UTF-16 code units", () => {
    expect(utf8ByteLength("abc")).toBe(3);
    expect(utf8ByteLength("café")).toBe(5);
    expect(utf8ByteLength("⚡")).toBe(3);
    expect(utf8ByteLength("\u{1d11e}")).toBe(4); // surrogate pair
  });

  it("counts lines with CRLF as a single terminator", () => {
    expect(countLines("")).toBe(1);
    expect(countLines("one")).toBe(1);
    expect(countLines(`a${LF}b`)).toBe(2);
    expect(countLines(`a${CR}${LF}b`)).toBe(2);
    expect(countLines(`a${CR}b`)).toBe(2);
    expect(countLines(`a${CR}${LF}b${LF}c${CR}`)).toBe(4); // trailing terminator opens a line
  });

  it("detects NUL characters", () => {
    expect(containsNul("plain")).toBe(false);
    expect(containsNul(`bad${NUL}value`)).toBe(true);
  });

  it("escapes every invisible class without touching visible text", () => {
    const esc = (s: string) => escapeCellText(s);
    expect(esc("plain text")).toBe("plain text");
    expect(esc(`a${LF}b`)).toBe(`a${BACKSLASH}n${LF}b`);
    expect(esc(`a${CR}${LF}b`)).toBe(`a${BACKSLASH}r${BACKSLASH}n${LF}b`);
    expect(esc(`a${TAB}b`)).toBe(`a${BACKSLASH}tb`);
    expect(esc(`a${NBSP}b`)).toBe(`a${BACKSLASH}u{00a0}b`);
    expect(esc(`a${ZWSP}b`)).toBe(`a${BACKSLASH}u{200b}b`);
    expect(esc(`a${REPLACEMENT}b`)).toBe(`a${BACKSLASH}u{fffd}b`);
    expect(esc(`a${BEL}b`)).toBe(`a${BACKSLASH}u{0007}b`);
    expect(esc(`a${NUL}b`)).toBe(`a${BACKSLASH}0b`);
    // Backslashes escape so the output is unambiguous.
    expect(esc(`C:${BACKSLASH}dir`)).toBe(`C:${BACKSLASH}${BACKSLASH}dir`);
  });

  it("escaping is display-only and clean strings pass through", () => {
    const clean = "regular value 123";
    expect(hasInvisibles(clean)).toBe(false);
    expect(hasInvisibles(`tab${TAB}here`)).toBe(true);
    expect(escapeCellText(clean)).toBe(clean);
  });
});
