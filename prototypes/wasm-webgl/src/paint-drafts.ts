export interface PaintDraft {
  id: string;
  revision: number;
  editorVersion: 1;
  templateId: string;
  templateVersion: number;
  layoutHash: string;
  modelId: string;
  modifiedAt: number;
  image: Blob;
}

export class DraftError extends Error {
  constructor(readonly code: 'unavailable' | 'conflict' | 'limit' | 'quota' | 'invalid', message: string) {
    super(message);
  }
}

const LIMIT_COUNT = 10;
const LIMIT_BYTES = 64 * 1024 * 1024;
const DB_NAME = 'ldw-paint-drafts';

function request<T>(source: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    source.onsuccess = () => resolve(source.result);
    source.onerror = () => reject(source.error);
  });
}

function complete(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error ?? new Error('IndexedDB transaction aborted'));
  });
}

async function database(): Promise<IDBDatabase> {
  if (!('indexedDB' in window)) throw new DraftError('unavailable', 'Локальное хранилище недоступно');
  return new Promise((resolve, reject) => {
    let open: IDBOpenDBRequest;
    try { open = indexedDB.open(DB_NAME, 1); }
    catch { reject(new DraftError('unavailable', 'Локальное хранилище недоступно')); return; }
    open.onupgradeneeded = () => open.result.createObjectStore('drafts', { keyPath: 'id' });
    open.onsuccess = () => resolve(open.result);
    open.onerror = () => reject(new DraftError('unavailable', 'Локальное хранилище недоступно'));
    open.onblocked = () => reject(new DraftError('unavailable', 'Локальное хранилище заблокировано другой вкладкой'));
  });
}

function validate(value: unknown): PaintDraft {
  const item = value as PaintDraft;
  if (!item || typeof item.id !== 'string' || !Number.isInteger(item.revision) || item.revision < 1 ||
      item.editorVersion !== 1 || typeof item.templateId !== 'string' ||
      !Number.isInteger(item.templateVersion) || typeof item.layoutHash !== 'string' ||
      !/^[a-f0-9]{64}$/.test(item.layoutHash) || typeof item.modelId !== 'string' ||
      !Number.isFinite(item.modifiedAt) || !(item.image instanceof Blob) ||
      item.image.type !== 'image/png' || item.image.size > LIMIT_BYTES)
    throw new DraftError('invalid', 'Черновик повреждён или имеет неподдерживаемый формат');
  return item;
}

function storageError(error: unknown): DraftError {
  if (error instanceof DraftError) return error;
  if (error instanceof DOMException && (error.name === 'QuotaExceededError' || error.name === 'UnknownError'))
    return new DraftError('quota', 'Место в браузере закончилось');
  return new DraftError('unavailable', 'Не удалось записать черновик на устройство');
}

export async function listDrafts(): Promise<PaintDraft[]> {
  const db = await database();
  try {
    const tx = db.transaction('drafts', 'readonly');
    const rows = await request(tx.objectStore('drafts').getAll());
    await complete(tx);
    return rows.map(validate).sort((a, b) => b.modifiedAt - a.modifiedAt);
  } finally { db.close(); }
}

export async function loadDraft(id: string): Promise<PaintDraft> {
  const db = await database();
  try {
    const tx = db.transaction('drafts', 'readonly');
    const row: unknown = await request(tx.objectStore('drafts').get(id));
    await complete(tx);
    if (!row) throw new DraftError('invalid', 'Черновик не найден');
    return validate(row);
  } finally { db.close(); }
}

export async function saveDraft(input: Omit<PaintDraft, 'revision' | 'modifiedAt'>,
                                expectedRevision: number): Promise<PaintDraft> {
  const db = await database();
  try {
    const tx = db.transaction('drafts', 'readwrite');
    const finished = complete(tx);
    try {
      const store = tx.objectStore('drafts');
      const rows = (await request(store.getAll())).map(validate);
      const previous = rows.find(row => row.id === input.id);
      if ((previous?.revision ?? 0) !== expectedRevision)
        throw new DraftError('conflict', 'Черновик изменён в другой вкладке');
      if (!previous && rows.length >= LIMIT_COUNT)
        throw new DraftError('limit', 'Достигнут лимит 10 черновиков');
      const used = rows.reduce((sum, row) => sum + (row.id === input.id ? 0 : row.image.size), 0);
      if (used + input.image.size > LIMIT_BYTES)
        throw new DraftError('limit', 'Достигнут лимит 64 МиБ черновиков');
      const saved: PaintDraft = { ...input, revision: expectedRevision + 1, modifiedAt: Date.now() };
      store.put(saved);
      await finished;
      return saved;
    } catch (error) {
      try { tx.abort(); } catch { /* Already completed or aborted. */ }
      void finished.catch(() => {});
      throw storageError(error);
    }
  } finally { db.close(); }
}

export async function deleteDraft(id: string, expectedRevision: number): Promise<void> {
  const db = await database();
  try {
    const tx = db.transaction('drafts', 'readwrite');
    const finished = complete(tx);
    try {
      const store = tx.objectStore('drafts');
      const previous: unknown = await request(store.get(id));
      if (!previous || validate(previous).revision !== expectedRevision)
        throw new DraftError('conflict', 'Черновик изменён в другой вкладке');
      store.delete(id);
      await finished;
    } catch (error) {
      try { tx.abort(); } catch { /* Already completed or aborted. */ }
      void finished.catch(() => {});
      throw storageError(error);
    }
  } finally { db.close(); }
}
