import { describe, expect, it } from 'vitest';
import { EditSession } from './edit-session.svelte';
import { commitMessageHeader, type OpAnswer } from './ontology';

const receipt = (seq: number): OpAnswer => ({
  inserted: 1,
  deleted: 0,
  receipt: {
    dataset: 'd',
    datasetId: '',
    committed: true,
    commit: { seq } as never,
  },
});

describe('the editing session', () => {
  it('keeps an undo stack of the commits it made and reloads after each', async () => {
    let reloads = 0;
    const s = new EditSession({ reload: async () => void reloads++ });
    let after = 0;
    s.edit({ kind: 'update', sparql: 'INSERT DATA {}' }, 'Add a label', () => after++);
    expect(s.previewOpen).toBe(true);
    expect(s.previewTitle).toBe('Add a label');
    await s.committed(s.previewOp!, receipt(7));
    expect(s.done.map((d) => d.commit)).toEqual([7]);
    expect(s.own.has(7)).toBe(true);
    expect(after).toBe(1);
    expect(reloads).toBe(1);

    // undo previews the revert of commit 7, and its revert becomes the redo
    s.undoLast();
    expect(s.previewOp).toEqual({ kind: 'revert', commit: 7 });
    expect(s.previewTitle).toBe('Undo: Add a label');
    await s.committed(s.previewOp!, receipt(8));
    expect(s.done).toEqual([]);
    expect(s.undone.map((d) => d.commit)).toEqual([8]);
    s.redoLast();
    expect(s.previewOp).toEqual({ kind: 'revert', commit: 8 });
    await s.committed(s.previewOp!, receipt(9));
    expect(s.done.map((d) => d.commit)).toEqual([9]);

    // a commit made outside the preview, such as a form's save, can be undone too
    s.record(10, 'Edit the notes');
    expect(s.done.map((d) => d.label)).toEqual(['Add a label', 'Edit the notes']);
    expect(s.undone).toEqual([]);
    s.reset('');
    expect(s.done).toEqual([]);
    expect(s.previewOpen).toBe(false);
  });
});

describe('commit messages', () => {
  it('are sent as they are when ASCII, and percent-encoded UTF-8 otherwise', () => {
    expect(commitMessageHeader('Add a label')).toBe('Add a label');
    expect(commitMessageHeader('Add “Baleines”@fr')).toBe(
      `utf-8''Add%20%E2%80%9CBaleines%E2%80%9D%40fr`,
    );
  });
});
