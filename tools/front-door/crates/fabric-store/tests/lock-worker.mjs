import { parentPort, workerData } from 'node:worker_threads';
import { createRequire } from 'node:module';
const Database = createRequire(workerData.source)('better-sqlite3');
const db = new Database(workerData.database);
db.exec('BEGIN IMMEDIATE');
parentPort.postMessage('locked');
parentPort.once('message', () => {
  db.exec('ROLLBACK');
  db.close();
  parentPort.postMessage('released');
  parentPort.close();
});
