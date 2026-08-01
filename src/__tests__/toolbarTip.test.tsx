import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ToolbarTip } from "../components/ToolbarTip";

describe("ToolbarTip", () => {
  it("uses only the app tooltip for its visible hover hint", () => {
    const markup = renderToStaticMarkup(
      <ToolbarTip label="Archive">
        <button type="button" title="Native archive hint" aria-describedby="native-hint">
          Archive icon
        </button>
      </ToolbarTip>,
    );

    expect(markup).toContain('aria-label="Archive"');
    expect(markup).toContain('role="tooltip"');
    expect(markup).not.toContain("Native archive hint");
    expect(markup).not.toContain("native-hint");
  });
});
