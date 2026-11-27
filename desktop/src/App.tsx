import { useEffect, useState } from 'react';
import { AnimatePresence, motion } from 'motion/react';

type Status = 'done' | 'running' | 'blocked' | 'pending';
type Step = { id: number; title: string; detail: string; status: Status; meta: string };
type Mode = 'ask' | 'auto' | 'dry';
type UiEvent = { kind: string; title: string; detail: string; status: Status; meta: string };
type PendingApproval = { id: string; tool: string; args: unknown; hash: string };
const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));

export default function App() {
  const [steps, setSteps] = useState<Step[]>([
    { id: 1, title: 'Plan', detail: 'submit_plan(3 steps) · Rust validated', status: 'done', meta: '0.4s' },
    { id: 2, title: 'Act', detail: 'read_file src/tax.rs offset=0 limit=120', status: 'done', meta: '182 tok' },
  ]);
  const [sessions] = useState(['Tax calculation', 'Report CSV cleanup', 'Audit dependency tree']);
  const [title, setTitle] = useState('Find why calc_total uses 8% tax');
  const [composer, setComposer] = useState('');
  const [mode, setMode] = useState<Mode>('ask');
  const [theme, setTheme] = useState<'light' | 'dark'>('light');
  const [approval, setApproval] = useState<'pending' | 'allowed' | 'denied'>('pending');
  const [running, setRunning] = useState(false);
  const [palette, setPalette] = useState(false);
  const [cache, setCache] = useState(0);
  const [resultCap, setResultCap] = useState<number | null>(null);
  const [workspace, setWorkspace] = useState('');
  const [verifyCommand, setVerifyCommand] = useState('cargo test --lib');
  const [pendingApproval, setPendingApproval] = useState<PendingApproval | null>(null);
  const [doneCondition, setDoneCondition] = useState('the verifier command exits 0');
  const [boundariesText, setBoundariesText] = useState('do not edit test files\nstay inside the workspace\nno network');
  const [clarifyOpen, setClarifyOpen] = useState(false);
  const [clarified, setClarified] = useState(false);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  // Prove the Tauri bridge is live: the cap comes from tinyloop-core, not a hardcoded UI value.
  useEffect(() => {
    if (!('__TAURI_INTERNALS__' in window)) return;
    import('@tauri-apps/api/core')
      .then(({ invoke }) => invoke<number>('context_budget'))
      .then(setResultCap)
      .catch(() => setResultCap(null));
  }, []);

  // Default the workspace to the core process's working directory instead of
  // shipping someone's local path in the UI.
  useEffect(() => {
    if (!isTauri) return;
    import('@tauri-apps/api/core')
      .then(({ invoke }) => invoke<string>('current_dir'))
      .then(dir => { if (typeof dir === 'string' && dir) setWorkspace(dir); })
      .catch(() => { /* leave blank; the user can type a path */ });
  }, []);

  // In the packaged app, core → UI events stream through Tauri.
  // In a browser preview this guard is false and the prototype simulation runs.
  useEffect(() => {
    if (!isTauri) return;
    let cancelled = false;
    let offs: Array<() => void> = [];
    import('@tauri-apps/api/event').then(async ({ listen }) => {
      const a = await listen<UiEvent>('run_event', event => {
        const e = event.payload;
        setSteps(s => [...s, { id: Date.now() + Math.random(), title: e.title, detail: e.detail, status: e.status, meta: e.meta }]);
      });
      const b = await listen<PendingApproval>('approval_required', event => setPendingApproval(event.payload));
      if (cancelled) { a(); b(); } else { offs = [a, b]; }
    });
    return () => { cancelled = true; offs.forEach(fn => fn()); };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); setPalette(p => !p); }
      if (e.key === 'Escape') setPalette(false);
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'd') { e.preventDefault(); setTheme(t => t === 'dark' ? 'light' : 'dark'); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const add = (s: Omit<Step, 'id'>) => setSteps(prev => [...prev, { ...s, id: Date.now() + Math.random() }]);

  const executeRun = async () => {
    if (running) return;
    setRunning(true);
    const text = composer.trim() || 'Run sample: repo tax investigation';
    setTitle(text); setComposer(''); setSteps([]); setPendingApproval(null);
    if (isTauri) {
      try {
        const { invoke } = await import('@tauri-apps/api/core');
        const report = await invoke<{ status?: string; steps: number; prompt_tokens: number; completion_tokens: number; cache_hits: number }>('run_task', {
          task: text,
          doneCondition,
          boundaries: boundariesText.split('\n').map(s => s.trim()).filter(Boolean),
          verifyCommand,
          cwd: workspace,
          mode,
          model: 'spark-x26',
        });
        setCache(report.cache_hits);
        setApproval(report.status === 'Done' ? 'allowed' : 'denied');
      } catch (error) {
        add({ title: 'Error', detail: String(error).slice(0, 400), status: 'blocked', meta: 'core' });
        setApproval('denied');
      }
      setRunning(false);
      return;
    }
    add({ title: 'Plan', detail: 'submit_plan(3 steps)', status: 'done', meta: '0.4s' });
    await sleep(360);
    add({ title: 'Act', detail: 'read_file src/tax.rs', status: 'done', meta: '182 tok' });
    await sleep(420);
    add({ title: 'Act', detail: 'search_files pattern="8%"', status: 'done', meta: '46 tok' });
    setCache(c => c + 2);
    await sleep(420);
    add({ title: 'Verify', detail: 'cargo test --quiet · independent judge', status: 'done', meta: 'verified' });
    setApproval('allowed'); setRunning(false);
  };

  const decide = async (decision: 'approve' | 'deny') => {
    if (!pendingApproval) return;
    const id = pendingApproval.id;
    setPendingApproval(null);
    setApproval(decision === 'approve' ? 'allowed' : 'denied');
    if (isTauri) {
      const { invoke } = await import('@tauri-apps/api/core');
      try { await invoke(decision, { id }); } catch { /* core will deny on timeout */ }
    }
  };

  const cancel = async () => {
    if (isTauri) {
      const { invoke } = await import('@tauri-apps/api/core');
      try { await invoke('cancel_run'); } catch { /* ignore */ }
    }
    setRunning(false);
  };

  const run = () => {
    if (running) return;
    if (!clarified) { setClarifyOpen(true); return; }
    void executeRun();
  };

  const mark = (s: Status) => s === 'done' ? '✓' : s === 'running' ? '⟳' : s === 'blocked' ? '!' : '·';

  return (
    <div className="desk"><div className="window">
      <header className="titlebar">
        <div className="traffic"><i className="dot r"/><i className="dot y"/><i className="dot g"/></div>
        <div className="brand"><span className="glyph">t</span>tinyloop</div>
        <span className="badge">Tauri shell</span>
        <div className="spacer"/>
        <div className="segmented">{(['ask','auto','dry'] as Mode[]).map(m => (
          <button key={m} aria-pressed={mode === m} onClick={() => setMode(m)}>{m === 'dry' ? 'Dry run' : m[0].toUpperCase() + m.slice(1)}</button>
        ))}</div>
        <div className="pill"><i className="live"/>Spark X2.6 · 4B · 8K</div>
        <button className="icon-btn" aria-label="Appearance" onClick={() => setTheme(t => t === 'dark' ? 'light' : 'dark')}>◐</button>
        {running && <button className="icon-btn" aria-label="Cancel run" onClick={cancel}>■</button>}
        <button className="run-btn" data-testid="run-button" disabled={running} onClick={run}>▶ Run</button>
      </header>

      <div className="body">
        <aside className="sidebar">
          <div><div className="side-label">Sessions</div>
            {sessions.map((s, i) => <button className="session" aria-selected={i === 0} key={s}>
              <span className="s-icon">{i + 1}</span><span><span className="s-title">{s}</span><span className="s-meta">bounded loop</span></span>
            </button>)}</div>
          <button className="new-btn" onClick={() => { setClarified(false); setClarifyOpen(true); }}>＋ New task</button>
          <div className="side-foot"><div className="side-label">Model</div>
            <div className="metric"><span>Cache hits</span><b>{cache}</b></div>
            <div className="metric"><span>Context</span><b>8192</b></div>
            <div className="metric"><span>Result cap</span><b>{resultCap ? resultCap.toLocaleString() : '…'} ch</b></div></div>
        </aside>

        <main className="main">
          <div className="main-head"><div className="crumb">Sessions / <span>current run</span></div>
            <h1>{title}</h1><div className="sub">Bounded loop · one tool call per turn · deterministic verifier</div></div>
          <div className="timeline"><AnimatePresence initial={false}>
            {steps.map(s => <motion.div key={s.id} className={'step ' + s.status} initial={{ opacity: 0, y: 8, filter: 'blur(3px)' }} animate={{ opacity: 1, y: 0, filter: 'none' }} exit={{ opacity: 0 }}>
              <div className="mark">{mark(s.status)}</div>
              <div><div className="s-title">{s.title}</div><div className="s-detail">{s.detail}</div></div>
              <div className="s-meta">{s.meta}</div>
            </motion.div>)}
          </AnimatePresence></div>
          <div className="composer"><div className="field">
            <textarea data-testid="task-input" rows={1} value={composer} placeholder="Describe a task for the agent…" onChange={e => setComposer(e.target.value)}
              onKeyDown={e => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void run(); } }} />
            <button className="send" aria-label="Send" onClick={() => void run()}>↑</button>
          </div><div className="hint"><span><kbd>↵</kbd> send</span><span><kbd>⌘K</kbd> commands</span></div></div>
        </main>

        <aside className="inspector">
          {isTauri && <div className="section"><h2>Run setup</h2><div className="contract">
            <label className="k">Workspace</label>
            <input className="run-input" data-testid="workspace-input" value={workspace} onChange={e => setWorkspace(e.target.value)} />
            <label className="k">Verifier</label>
            <input className="run-input" data-testid="verifier-input" value={verifyCommand} onChange={e => setVerifyCommand(e.target.value)} />
            <p className="k" style={{ marginTop: 6 }}>Ask mode blocks writes until approval is wired; use Auto for a full run.</p>
          </div></div>}
          <div className="section"><h2>Goal</h2><div className="contract"><p><span className="k">Done when</span> {doneCondition}</p></div></div>
          <div className="section"><h2>Boundaries</h2><div className="contract"><p><span className="k">Must not</span> {boundariesText.split('\n').map(s => s.trim()).filter(Boolean).join('; ') || 'none set'}</p></div></div>
          <div className="section"><h2>Verifier</h2><div className="contract"><p><code>{verifyCommand || 'none — the loop cannot declare Done without one'}</code><br/><span className="k">Judge</span><code>Rust, not the model</code></p></div></div>
          <div className="divider"/>
          <div className="section"><h2>Approval</h2>
            {pendingApproval ? <div className="approval" data-testid="approval-card"><div className="head"><i className="dot2"/>Approval required</div>
              <div className="cmd">{pendingApproval.tool} {JSON.stringify(pendingApproval.args)}</div>
              <div className="hash">bound to sha256:{pendingApproval.hash.slice(0, 8)}…{pendingApproval.hash.slice(-4)}</div>
              <div className="actions"><button className="btn primary" data-testid="allow-button" onClick={() => void decide('approve')}>Allow</button>
                <button className="btn quiet" data-testid="deny-button" onClick={() => void decide('deny')}>Deny</button></div></div>
              : <div className="resolved"><span className="ok">{approval === 'allowed' ? '✓' : approval === 'denied' ? '×' : '·'}</span> {approval === 'allowed' ? 'Approved and bound' : approval === 'denied' ? 'Denied — no changes made' : 'Waiting for a gate'}</div>}
          </div>
        </aside>
      </div>

      {clarifyOpen && <div className="palette open" onClick={() => setClarifyOpen(false)}><div className="pbox clarify" onClick={e => e.stopPropagation()}>
        <h3>Confirm the goal</h3>
        <label className="k">Done when</label>
        <textarea className="clarify-input" rows={3} value={doneCondition} onChange={e => setDoneCondition(e.target.value)} />
        <label className="k">Boundaries — one per line</label>
        <textarea className="clarify-input" rows={4} value={boundariesText} onChange={e => setBoundariesText(e.target.value)} />
        <div className="actions">
          <button className="btn quiet" onClick={() => setClarifyOpen(false)}>Cancel</button>
          <button className="btn primary" data-testid="clarify-confirm" onClick={() => { setClarified(true); setClarifyOpen(false); void executeRun(); }}>Confirm &amp; Run</button>
        </div>
      </div></div>}

      {palette && <div className="palette open" onClick={() => setPalette(false)}><div className="pbox" onClick={e => e.stopPropagation()}>
        <input placeholder="Type a command…" autoFocus aria-label="Command"/>
        <button className="pitem" onClick={() => setPalette(false)}><span>New task</span><span>⌘N</span></button>
        <button className="pitem" onClick={() => setTheme(t => t === 'dark' ? 'light' : 'dark')}><span>Toggle appearance</span><span>⌘D</span></button>
        <button className="pitem" onClick={() => { setMode('dry'); setPalette(false); }}><span>Dry-run mode</span><span>⌘⇧D</span></button>
      </div></div>}
    </div></div>
  );
}
