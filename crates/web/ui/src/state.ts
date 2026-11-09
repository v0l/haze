import { createContext } from "preact";
import { useContext } from "preact/hooks";
import type { Range, Route } from "./route";
import type { TargetView } from "./types";

export type Theme = "dark" | "light";

export interface Dash {
  route: Route;
  range: Range;
  now: number;
  theme: Theme;
  targets: TargetView[];
  alertsVersion: number;
  setRange: (range: Range) => void;
  navigate: (route: Route, range?: Range) => void;
}

export const DashContext = createContext<Dash | null>(null);

export function useDash(): Dash {
  const dash = useContext(DashContext);
  if (!dash) throw new Error("useDash outside the dashboard");
  return dash;
}
