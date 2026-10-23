// Run the source Store unchanged. Only time and UUID entropy are fixture inputs.
import crypto from 'node:crypto';
import { syncBuiltinESMExports, createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import { writeFileSync } from 'node:fs';
const [source, database, scenario] = process.argv.slice(2);
let now = 1000, serial = 0;
Date.now = () => now;
crypto.randomUUID = () => `fixture-${++serial}`;
syncBuiltinESMExports();
const { Store, inspectDatabase, laneTaskId } = await import(pathToFileURL(source).href);
const Database = createRequire(pathToFileURL(source))('better-sqlite3');
const a = { project: 'p', agentId: 'alice', provider: 'codex' };
const b = { ...a, agentId: 'bob' };
const out = [];
const capture = (f) => {
  let entry; try { entry = {ok: f() ?? null}; } catch (e) { entry = {error: e.message}; }
  if (entry.ok?.database) entry.ok.database = '<fixture-db>';
  out.push({...entry, wire: JSON.stringify(entry)});
};
if (scenario === 'legacy') {
  const db = new Database(database);
  db.exec(`CREATE TABLE messages(message_id TEXT PRIMARY KEY, project TEXT NOT NULL, sender_id TEXT NOT NULL,
    body TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'note', conversation_id TEXT NOT NULL,
    reply_to TEXT REFERENCES messages(message_id), created_at INTEGER NOT NULL);
    INSERT INTO messages VALUES ('old', 'p', 'alice', 'retained', 'note', 'old', NULL, 1000)`);
  db.close();
}
const store = new Store(database);
const other = new Store(database);
const raw = new Database(database);
if (scenario === 'legacy-rows') {
  const rows=[{}, {run_id:null,task_id:'',attempt_count:'2'}, {run_id:42,task_id:0,attempt:null,attempt_count:true},
    {run_id:['r',null,['x']],task_id:[],attempt:[]}, {run_id:{},task_id:'t',attempt:'2.5'},
    {run_id:'r',task_id:'t',attempt:'0x10000000000000001'}, {run_id:'r',attempt:'invalid'},
    {run_id:'r',attempt:'Infinity'}, {run_id:'r',attempt:'-Infinity'},
    {run_id:{toString:null},attempt:1}, {run_id:'r',attempt:1,run_dir:false},
    {run_id:'r',attempt:null}, {run_id:'r',attempt:'',run_dir:null},
    {run_id:1e21,attempt:1},{run_id:1e-7,attempt:1},{run_id:1e20,attempt:1},
    {run_id:true,attempt:['3']},{run_id:1e-6,attempt:1}];
  for (const row of rows) {capture(()=>laneTaskId(row));capture(()=>store.acknowledgeTerminal(b,row,1));}
  const lanes=[{runId:'r',taskId:'t',attempt:3},{runId:'r',taskId:'t',attempt:2.5},{runId:'zero',taskId:'t',attempt:0}];
  capture(()=>store.unseenLanes(b,lanes));capture(()=>store.markLanesSeen(b,lanes));
  capture(()=>store.unseenLanes(b,lanes));
  store.announce(a);store.announce(b);
  capture(()=>store.send(a,'bob','fraction seen',{kind:'run_terminal',outputPath:'r:t:2.5'}));
} else if (scenario === 'ids') {
  store.announce(a); store.announce(b);
  capture(() => store.send(a,'bob','empty task',{taskId:''}));
  capture(() => store.send(a,'bob','empty output',{outputPath:''}));
  capture(() => store.send(a,'missing','failed recipient'));
  capture(() => store.claimSessionTurn(a,'name',null,'start',123));
  capture(() => store.claimSessionTurn(a,'name',null,'start',124));
  capture(() => store.createTask(a,'generated'));
  capture(() => store.createTask(a,'empty',{taskId:''}));
  capture(() => store.send(a,'bob','generated'));
  capture(() => store.inbox(b));
} else if (scenario === 'work') {
  store.announce(a); store.announce(b); store.announce({...a,project:'z'});
  capture(() => store.agents('p')); capture(() => store.projects());
  capture(() => store.acquireWork(a,'s',{issue:' #869 ',paths:['Runtime\\Fabric/','runtime/fabric']},1,1000));
  capture(() => store.verifyWork(a,'s','fixture-1',1,1000));
  capture(() => other.acquireWork(b,'s',{issue:'869'},60,1000));
  capture(() => other.acquireWork(b,'s',{paths:['RUNTIME/FABRIC/SRC']},60,1000));
  capture(() => store.renewWork(a,'wrong','fixture-1',1,60,1000));
  capture(() => store.renewWork(a,'s','fixture-1',1,1,1000));
  capture(() => other.acquireWork(b,'s',{paths:['runtime/fabric/src']},60,2000));
  capture(() => store.renewWork(a,'s','fixture-1',1,60,2000));
  capture(() => store.verifyWork(a,'s','fixture-1',1,2000));
  capture(() => store.releaseWork(a,'s','fixture-1',1,2000));
  capture(() => store.workClaims('p',2000));
  capture(() => other.releaseWork(b,'s','fixture-4',2,2000));
  for (const path of ['', '/', 'a//b','a/../b','./a','a//']) capture(() => store.acquireWork(a,'s',{paths:[path]},60,2000));
  capture(() => store.acquireWork(a,'s',{issue:'#'},60,2000));
  capture(() => store.acquireWork(a,'s',{paths:Array(101).fill('a')},60,2000));
  capture(() => store.acquireWork(a,'bad/session',{issue:'x'},60,2000));
  capture(() => store.acquireWork(a,'s',{issue:'x'},0,2000));
  raw.exec("CREATE TRIGGER fail_log BEFORE INSERT ON activity BEGIN SELECT RAISE(ABORT,'log blocked'); END");
  capture(() => store.acquireWork(a,'s',{paths:['different']},60,2000));
  raw.exec('DROP TRIGGER fail_log');
  capture(() => store.workClaims('p',2000));
} else if (scenario === 'review') {
  store.announce(a);
  capture(() => store.announce({...a, provider:'claude'}));
  capture(() => store.createTeam(a, 'future', ['alice']));
  capture(() => store.announce({...a, agentId:'future'}));
  capture(() => store.announce({...a, agentId:'all'}));
  for (const agentId of ['chair','leader','parent-seat']) store.announce({...a, agentId});
  process.env.PROVENANT_CHAIR='leader'; process.env.PROVENANT_PARENT='parent-seat';
  for (const alias of ['chair','parent','root','/root']) capture(() => store.send(a, alias, 'question'));
  process.env.PROVENANT_CHAIR='missing'; process.env.PROVENANT_PARENT='missing';
  capture(() => store.send(a, 'parent', 'fallback'));
  const sha='a'.repeat(40);
  capture(() => store.acquireLanding(a,'s',sha,1,1000));
  raw.exec('UPDATE landing_leases SET pushing_until=151000');
  capture(() => other.acquireLanding(b,'s',sha,60,2001));
  capture(() => store.tryReleaseLanding(a,'s',1,2001));
  capture(() => other.acquireLanding(b,'s',sha,60,151000));
} else if (scenario === 'waits') {
  const asyncCapture = async (f) => {
    let entry; try { entry={ok:await f()}; } catch(e) { entry={error:e.message}; }
    out.push({...entry,wire:JSON.stringify(entry)});
  };
  let onWait=()=>{};
  globalThis.setTimeout=(done,ms)=>{ now+=ms; onWait(); done(); };
  await asyncCapture(() => store.waitLanding('p',0.003,undefined,1));
  await asyncCapture(() => store.waitLanding('p',1,AbortSignal.abort(),1));
  now=1000;
  store.acquireLanding(a,'s','a'.repeat(40),60,now);
  let polls=0;
  onWait=()=>{ if(++polls===1) raw.exec('UPDATE landing_leases SET pushing_until=151000');
    else raw.exec('UPDATE landing_leases SET released_at=1002'); };
  await asyncCapture(() => store.waitLanding('p',1,undefined,1));
  store.acquireLanding(a,'s','a'.repeat(40),1,now);
  onWait=()=>{};
  await asyncCapture(() => store.waitLanding('p',2,undefined,500));
  store.acquireLanding(a,'s','a'.repeat(40),60,now);
  onWait=()=>{ raw.exec('UPDATE landing_leases SET generation=generation+1'); };
  await asyncCapture(() => store.waitLanding('p',1,undefined,1));
} else if (scenario === 'thread-contention') {
  const { Worker } = await import('node:worker_threads');
  const { once } = await import('node:events');
  store.announce(a); store.announce(b); store.send(a,'bob','claim me');
  const worker=new Worker(new URL('./lock-worker.mjs',import.meta.url), {workerData:{source,database}});
  try {
    await once(worker,'message');
    capture(() => store.inbox(b,{busyTimeoutMs:1}));
    capture(() => store.acknowledgeTerminal(b,{run_id:'r',task_id:'t',attempt:1,run_dir:'/r'},1));
    const released=once(worker,'message'); worker.postMessage('release'); await released;
  } finally { await worker.terminate(); }
  capture(() => store.inbox(b));
} else if (scenario === 'leases') {
  const sha = 'a'.repeat(40);
  capture(() => store.acquireLanding(a, 'a', sha, 1, 1000));
  capture(() => other.acquireLanding(b, 'b', sha, 60, 1000));
  capture(() => store.verifyLanding(a, 'a', 1, 'b'.repeat(40), 1000));
  capture(() => store.renewLanding(a, 'a', 1, 1, 1000));
  capture(() => other.acquireLanding(b, 'b', sha, 60, 2000));
  capture(() => store.verifyLanding(a, 'a', 1, sha, 2000));
  capture(() => store.tryReleaseLanding(a, 'a', 1, 2000));
  now = 2000;
  capture(() => other.withLandingPush(b, 'b', 2, sha, () => {
    store.note(a, 'write during push');
    capture(() => store.tryReleaseLanding(b, 'b', 2, 2000));
    capture(() => store.withLandingPush(b, 'b', 2, sha, () => 'forbidden'));
    throw new Error('push failed');
  }));
  capture(() => other.landingLease('p', 2000));
  capture(() => other.withLandingPush(b, 'b', 2, sha, () => 'pushed'));
  capture(() => other.landingLease('p', 2000));
  capture(() => store.acquireLanding(a, 'c', sha, 1, 2000));
  capture(() => store.tryReleaseLanding(a, 'c', 3, 3000));
  capture(() => store.acquireLanding(a, 'bad/name', sha, 1, 2000));
  capture(() => store.acquireLanding(a, 'bad', sha.toUpperCase(), 1, 2000));
  capture(() => store.acquireLanding(a, 'bad', sha, 0, 2000));
  capture(() => store.acquireLanding(a, '\uFEFF', sha, 1, 2000));
  // A commit failure rolls back both the lease and activity.
  raw.exec(`CREATE TRIGGER fail_log BEFORE INSERT ON activity BEGIN SELECT RAISE(ABORT, 'log blocked'); END`);
  capture(() => store.renewLanding(a, 'c', 3, 60, 2000));
  raw.exec('DROP TRIGGER fail_log');
  capture(() => store.withLandingPush(a, 'c', 3, sha, () => {
    raw.exec(`CREATE TRIGGER fail_release BEFORE UPDATE OF released_at ON landing_leases BEGIN SELECT RAISE(ABORT, 'release blocked'); END`);
    return 'pushed';
  }));
  raw.exec('DROP TRIGGER fail_release');
} else if (scenario === 'sessions') {
  capture(() => store.claimSessionTurn(a, 'Review', null, 'start', 123));
  capture(() => other.claimSessionTurn(b, 'Review', null, 'start', 124));
  capture(() => store.bindSessionTurn(a, 'Review', 'wrong', {runId:'r',taskId:'t',attempt:1}));
  capture(() => store.bindSessionTurn(a, 'Review', 'fixture-1', {runId:'r',taskId:'t',attempt:1}));
  capture(() => store.releaseSessionLauncher(a, 'Review', 'fixture-1'));
  capture(() => store.settleSessionTurn(a, 'Review', 'wrong', 'failed'));
  capture(() => store.forgetSession(a, 'Review'));
  capture(() => store.settleSessionTurn(a, 'Review', 'fixture-1', 'ok', {adapter:'codex',providerSessionId:'native',runId:'r',taskId:'t',attempt:1,resultPath:'/result'}));
  capture(() => other.claimSessionTurn(b, 'Review', 'old-claim', 'resume', 125));
  capture(() => other.claimSessionTurn(b, 'Review', 'fixture-1', 'resume', 125));
  capture(() => other.settleSessionTurn(b, 'Review', 'fixture-4', 'failed'));
  capture(() => store.session('p', 'Review'));
  capture(() => store.session('p', 'review'));
  capture(() => store.sessions('p'));
  capture(() => store.forgetSession(a, 'Review'));
  capture(() => store.claimSessionTurn(a, 'NeverLaunched', null, 'fresh', 126));
  capture(() => store.settleSessionTurn(a, 'NeverLaunched', 'fixture-5', 'failed'));
  capture(() => store.sessions('p'));
  capture(() => store.claimSessionTurn(a, 'Missing', 'stale', 'start', 127));
} else if (scenario === 'messages') {
  store.announce(a); store.announce(b);
  capture(() => store.createTeam(a, 'all', []));
  capture(() => store.createTeam(a, 'bob', []));
  capture(() => store.createTeam(a, 'team', ['bob', 'alice', 'bob']));
  capture(() => store.createTask(a, 'work', {taskId:'task', dependsOn:['z', 'a', 'z']}));
  capture(() => store.send(a, 'missing', 'refused'));
  capture(() => store.send(a, 'bob', 'missing parent', {replyTo:'missing'}));
  capture(() => store.send(a, 'bob', 'missing task', {taskId:'missing'}));
  capture(() => store.send(a, 'team', 'hello\nworld', {taskId:'task'}));
  capture(() => store.send(a, 'all', 'reply', {replyTo:'fixture-4'}));
  capture(() => store.inbox(b, {peek:true, ids:[]}));
  capture(() => store.inbox(b, {peek:true}));
  capture(() => store.inboxDigest(b));
  capture(() => store.inbox(b, {claimTtlMs:50, busyTimeoutMs:1}));
  capture(() => store.inbox(b));
  capture(() => store.acknowledge(b, 'fixture-4', 'wrong'));
  now = 1050;
  capture(() => store.acknowledge(b, 'fixture-4', 'fixture-6'));
  capture(() => store.inbox(b, {ids:['fixture-4']}));
  capture(() => store.acknowledge(b, 'fixture-4', 'fixture-6'));
  capture(() => store.acknowledge(b, 'fixture-4', 'fixture-8'));
  capture(() => store.acknowledge(b, 'fixture-4', 'fixture-8'));
  capture(() => store.inbox(b, {peek:true}));
  capture(() => store.claimTask(a, 'task'));
  capture(() => store.claimTask(a, 'task'));
  capture(() => other.claimTask(b, 'task'));
  capture(() => store.updateTask(a, 'task', 'claimed'));
  capture(() => store.updateTask(a, 'task', 'done', ''));
  capture(() => store.claimTask(a, 'task'));
  capture(() => store.tasks('p'));
  capture(() => store.tasks('p', 'open'));
  capture(() => store.claimTask(a, 'missing'));
  capture(() => store.updateTask(a, 'missing', 'done'));
  capture(() => store.activityAfter('p', 2, 3));
  capture(() => store.activity('p', 2));
  capture(() => store.markLanesSeen(b, [{runId:'r',taskId:'t',attempt:2}]));
  capture(() => store.markLanesSeen(b, [{runId:'r',taskId:'t',attempt:1}]));
  capture(() => store.unseenLanes(b, [{runId:'r',taskId:'t',attempt:1},{runId:'r',taskId:'t',attempt:3},{runId:'r',taskId:'zero',attempt:0},{runId:'',taskId:'t',attempt:1}]));
  capture(() => store.send(a, 'bob', 'already observed', {kind:'run_terminal',outputPath:'r:t:2'}));
  capture(() => store.send(a, 'bob', 'new attempt', {kind:'run_terminal',outputPath:'r:t:3'}));
  capture(() => store.acknowledgeTerminal(b, {run_id:'r',task_id:'t',attempt:3,run_dir:'/legacy'}, 1));
  capture(() => store.inbox(b, {peek:true}));
  capture(() => store.inbox(b, {busyTimeoutMs:0}));
  capture(() => store.inbox(b, {limit:0}));
  capture(() => store.inbox(b, {claimTtlMs:0}));
  capture(() => store.inbox(b, {taskId:''}));
  capture(() => store.activityAfter('p', -1));
  capture(() => store.activity('p', 0));
} else if (scenario === 'dates') {
  store.note(a, 'date fixture');
  for (const at of [-8640000000000001, -8640000000000000, -62167219200000, -1, 0, 951782400000, 253402300800000, 8640000000000000, 8640000000000001]) {
    raw.prepare('UPDATE activity SET at=?').run(at);
    capture(() => store.activity('p', 50));
  }
} else if (scenario === 'contention') {
  store.announce(a); store.announce(b); store.send(a, 'bob', 'claim me');
  raw.exec('BEGIN IMMEDIATE');
  capture(() => store.inbox(b, {busyTimeoutMs:1}));
  capture(() => store.acknowledgeTerminal(b, {run_id:'r',task_id:'t',attempt:1,run_dir:'/r'},1));
  raw.exec('ROLLBACK');
  capture(() => store.inbox(b));
} else if (scenario === 'legacy') {
  capture(() => store.inbox(a, {peek:true}));
} else if (scenario === 'diagnostics') {
  store.announce(a); store.note(a, 'seed');
  capture(() => inspectDatabase(database, 'p', 'doctor'));
  raw.exec(`INSERT INTO teams VALUES ('p','alice',1000)`);
  capture(() => inspectDatabase(database, 'p', 'doctor'));
}
const schema = raw.prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name").all();
const tables = raw.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").all();
const rows = Object.fromEntries(tables.map(({name}) => [name, raw.prepare(`SELECT * FROM "${name}" ORDER BY rowid`).all()]));
// The database path is an explicit fixture binding; preserve every other field.
for (const entry of out) if (entry.ok?.database) entry.ok.database = '<fixture-db>';
console.log(JSON.stringify({out, schema, rows, pragmas:{busy_timeout:5000,journal_mode:raw.pragma('journal_mode',{simple:true}),foreign_keys:raw.pragma('foreign_keys',{simple:true})}}));
raw.close(); other.close(); store.close();
