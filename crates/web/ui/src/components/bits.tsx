import type { ComponentChildren } from "preact";
import { useDash } from "../state";
import type { Status } from "../types";

export function StatusDot({ status, title }: { status: Status; title?: string }) {
  return <span class={`dot ${status}`} title={title ?? status} aria-label={status} />;
}

export function TagChips({ tags, active }: { tags: string[]; active: string | null }) {
  const { navigate } = useDash();
  return (
    <>
      {tags.map((tag) => (
        <a
          key={tag}
          class={`tag${tag === active ? " on" : ""}`}
          href="#"
          onClick={(e) => {
            e.preventDefault();
            navigate({ page: "tree", tag: tag === active ? null : tag });
          }}
        >
          {tag}
        </a>
      ))}
    </>
  );
}

export function Card({ title, children }: { title?: string; children: ComponentChildren }) {
  return (
    <section class="card">
      {title ? <h2>{title}</h2> : null}
      {children}
    </section>
  );
}

const RANK: Record<Status, number> = { down: 4, alert: 3, loss: 2, ok: 1, unknown: 0 };

export function worst(statuses: Status[]): Status {
  return statuses.reduce<Status>((w, s) => (RANK[s] > RANK[w] ? s : w), "unknown");
}

export function SummaryPills({ statuses }: { statuses: Status[] }) {
  const counts: Partial<Record<Status, number>> = {};
  for (const s of statuses) counts[s] = (counts[s] ?? 0) + 1;
  const order: Status[] = ["down", "alert", "loss", "ok", "unknown"];
  return (
    <div class="summary">
      {order
        .filter((s) => counts[s])
        .map((s) => (
          <span class="pill" key={s}>
            <StatusDot status={s} />
            {`${counts[s]} ${s}`}
          </span>
        ))}
    </div>
  );
}
