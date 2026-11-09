import { Graph } from "../components/Graph";
import { RangeBar } from "../components/RangeBar";
import { BackoffNote } from "../components/Tables";
import { StatusDot, SummaryPills, TagChips, worst } from "../components/bits";
import { fmt, pct, probeLabel } from "../format";
import { useDash } from "../state";
import type { Status, TargetView } from "../types";

interface Node {
  children: Map<string, Node>;
  targets: TargetView[];
}

function treeOf(targets: TargetView[]): Node {
  const root: Node = { children: new Map(), targets: [] };
  for (const t of targets) {
    let node = root;
    for (const part of t.path.split("/").slice(0, -1)) {
      let next = node.children.get(part);
      if (!next) {
        next = { children: new Map(), targets: [] };
        node.children.set(part, next);
      }
      node = next;
    }
    node.targets.push(t);
  }
  return root;
}

function statuses(node: Node): Status[] {
  return [...node.targets.map((t) => t.status), ...[...node.children.values()].flatMap(statuses)];
}

function Row({ t, tag }: { t: TargetView; tag: string | null }) {
  const { navigate } = useDash();
  const open = () => navigate({ page: "target", target: t.path });
  return (
    <li class="row" data-target={t.path}>
      <a
        class="name"
        href="#"
        onClick={(e) => {
          e.preventDefault();
          open();
        }}
      >
        <StatusDot status={t.status} />
        <span class="label">{t.path.split("/").pop()}</span>
        <span class="muted small address">{probeLabel(t)}</span>
        <BackoffNote backoff={t.backoff} short />
      </a>
      <span class="tags">
        <TagChips tags={t.tags} active={tag} />
      </span>
      <span class="stat small">
        {t.last ? `${fmt(t.last.median_ms)} ms` : "-"}
        <span class="muted">{t.last ? ` ${pct(t.last.loss)}` : ""}</span>
      </span>
      <Graph target={t.path} height={34} small onOpen={open} />
    </li>
  );
}

function Branch({ name, node, tag }: { name: string | null; node: Node; tag: string | null }) {
  const list = (
    <ul class="tree">
      {node.targets.map((t) => (
        <Row key={t.path} t={t} tag={tag} />
      ))}
      {[...node.children].map(([child, sub]) => (
        <li key={child}>
          <Branch name={child} node={sub} tag={tag} />
        </li>
      ))}
    </ul>
  );
  if (name === null) return list;
  return (
    <details open class="group-node">
      <summary>
        <StatusDot status={worst(statuses(node))} />
        <span class="label">{name}</span>
      </summary>
      {list}
    </details>
  );
}

export function TreePage({ tag }: { tag: string | null }) {
  const { targets } = useDash();
  const tags = [...new Set(targets.flatMap((t) => t.tags))].sort();
  const shown = tag ? targets.filter((t) => t.tags.includes(tag)) : targets;
  return (
    <>
      <div class="head">
        <h1>Targets</h1>
        <SummaryPills statuses={targets.map((t) => t.status)} />
      </div>
      <RangeBar />
      {tags.length ? (
        <div class="tagbar">
          <span class="muted small">Tags</span>
          <TagChips tags={tags} active={tag} />
        </div>
      ) : null}
      {shown.length ? <Branch name={null} node={treeOf(shown)} tag={tag} /> : <p class="muted">No targets in the config.</p>}
    </>
  );
}
