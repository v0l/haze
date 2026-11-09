export type Status = "ok" | "loss" | "alert" | "down" | "unknown";

export interface LastStep {
  at_ms: number;
  sent: number;
  received: number;
  loss: number;
  min_ms: number | null;
  median_ms: number | null;
  p90_ms: number | null;
  max_ms: number | null;
  jitter_ms: number | null;
}

export interface TargetView {
  path: string;
  title: string | null;
  probe: string;
  address: string;
  tags: string[];
  alerts: string[];
  depends: string[];
  step_ms: number;
  pings: number;
  status: Status;
  active: string[];
  last: LastStep | null;
}

export interface TargetsResponse {
  targets: TargetView[];
  now: number;
}

export interface Point {
  t: number;
  steps: number;
  sent: number;
  received: number;
  loss: number;
  min: number | null;
  p10: number | null;
  p25: number | null;
  median: number | null;
  p75: number | null;
  p90: number | null;
  max: number | null;
}

export interface Series {
  target: string;
  from: number;
  to: number;
  step: number;
  points: Point[];
}

export type AlertEventName = "raised" | "cleared" | "flapping" | "settled-active" | "settled-clear" | "restart";

export interface AlertRecord {
  at_ms: number;
  target: string;
  alert: string;
  event: AlertEventName;
  comment?: string;
}

export interface Incident {
  target: string;
  alert: string;
  started_ms: number;
  ended_ms: number | null;
  flapping: boolean;
  comment?: string;
  end_reason?: string;
}

export interface AlertsResponse {
  incidents: Incident[];
  history: AlertRecord[];
}

export type LiveEvent =
  | { kind: "step"; target: string; status: Status; last: LastStep }
  | { kind: "alert"; target: string; status: Status; record: AlertRecord };
