import { cloneElement, type ReactElement } from "react";
import { ui } from "../theme";

type TooltipChildProps = {
  "aria-label"?: string;
  "aria-describedby"?: string;
  title?: string;
};

export function ToolbarTip({ label, children }: { label: string; children: ReactElement<TooltipChildProps> }) {
  return (
    <div className="group/tip relative inline-flex">
      {cloneElement(children, {
        "aria-label": children.props["aria-label"] ?? label,
        // WebView2 can surface native hover UI for title/description metadata.
        // The visible hint is owned by this component, so keep only the
        // accessible name on the control and suppress competing native hints.
        "aria-describedby": undefined,
        title: undefined,
      })}
      <span
        className={`pointer-events-none absolute left-1/2 z-[200] mt-1.5 w-max max-w-[220px] -translate-x-1/2 top-full text-center leading-tight opacity-0 transition-opacity duration-150 delay-75 group-hover/tip:opacity-100 group-focus-within/tip:opacity-100 ${ui.tooltip}`}
        role="tooltip"
      >
        {label}
      </span>
    </div>
  );
}
