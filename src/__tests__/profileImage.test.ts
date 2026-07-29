import { describe, expect, it } from "vitest";
import { proxiedProfileImageUrl } from "../profileImage";

describe("profile image URLs", () => {
  it("routes remote avatars through the validated local image proxy", () => {
    const result = proxiedProfileImageUrl("https://lh3.googleusercontent.com/a/example=s96-c");
    expect(result).toContain("http://mailimg.localhost/?url=");
    expect(result).toContain(encodeURIComponent("https://lh3.googleusercontent.com/a/example=s96-c"));
  });

  it("changes the request URL for a bounded retry", () => {
    expect(proxiedProfileImageUrl("https://example.test/avatar.png", 1)).toContain("&attempt=1");
  });

  it("rejects unsupported and malformed sources", () => {
    expect(proxiedProfileImageUrl("data:image/png;base64,abc")).toBeNull();
    expect(proxiedProfileImageUrl("not a URL")).toBeNull();
    expect(proxiedProfileImageUrl("")).toBeNull();
  });
});
