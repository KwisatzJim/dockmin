import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { render, screen, fireEvent, waitFor, cleanup } from '@testing-library/svelte';
import App from './App.svelte';

let data, calls;
beforeEach(() => {
  calls = [];
  data = {
    hosts: [{ id: 'h', name: 'Office', alias: 'office' }],
    stacks: ['media', 'books'].map(id => ({ id, host_id: 'h', name: id, directory: `/srv/${id}`, project: id, files: [] })),
    jobs: [],
    statuses: Object.fromEntries(['media', 'books'].map(id => [id, { checked: Math.floor(Date.now()/1000), services: [{ Service: id, State: 'running', Health: 'healthy' }] }])),
    updates: { media: { state: 'available', checked: 1, output: 'media: update available — new' }, books: { state: 'current', checked: 1, output: 'books: up to date — same' } },
  };
  vi.stubGlobal('fetch', vi.fn(async (url, options = {}) => {
    const body = options.body ? JSON.parse(options.body) : null;
    calls.push({ url, body });
    let response;
    if (url === '/api/session' || url === '/api/login') response = { csrf: 'csrf' };
    else if (url === '/api/overview') response = data;
    else if (url === '/api/bulk') {
      if (body.action === 'check-updates') {
        data = { ...data, updates: Object.fromEntries(body.stack_ids.map(id => [id, { state: 'queued' }])) };
      }
      response = { jobs: body.stack_ids.map(id => ({ id: `job-${id}` })) };
    } else throw new Error(`Unexpected request ${url}`);
    return { ok: true, status: 200, text: async () => JSON.stringify(response) };
  }));
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value() { this.setAttribute('open', ''); } });
  Object.defineProperty(HTMLDialogElement.prototype, 'close', { configurable: true, value() { this.removeAttribute('open'); } });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

test('defaults to Overview with a brief summary and update status for every stack', async () => {
  render(App);
  await screen.findByRole('heading', { name: 'Overview', exact: true });
  expect(screen.getByRole('heading', { name: 'media', exact: true })).toBeTruthy();
  expect(screen.getByRole('heading', { name: 'books', exact: true })).toBeTruthy();
  expect(screen.getAllByText('1/1 services running')).toHaveLength(2);
  expect(screen.getByText('Update available', { exact: true })).toBeTruthy();
  expect(screen.getByText('Up to date', { exact: true })).toBeTruthy();
});

test('Check All stays on Overview and shows progress without opening an output panel', async () => {
  render(App);
  await screen.findByRole('heading', { name: 'Overview', exact: true });
  await fireEvent.click(screen.getByRole('button', { name: 'Check All for Updates', exact: true }));
  await waitFor(() => expect(calls.some(c => c.body?.action === 'check-updates')).toBe(true));
  expect(calls.find(c => c.body?.action === 'check-updates').body.stack_ids).toEqual(['media', 'books']);
  expect(screen.getByRole('heading', { name: 'Overview', exact: true })).toBeTruthy();
  expect(screen.queryByRole('heading', { name: 'Operation history' })).toBeNull();
  expect(screen.queryByRole('region', { name: 'Operation output' })).toBeNull();
  await waitFor(() => expect(screen.getAllByText('Check queued')).toHaveLength(2));
  expect(screen.getByRole('button', { name: 'Update All with Updates (0)', exact: true }).disabled).toBe(true);
});

test('Update All confirms the targets and submits only stacks with confirmed updates', async () => {
  render(App);
  await screen.findByRole('heading', { name: 'Overview', exact: true });
  await fireEvent.click(screen.getByRole('button', { name: 'Update All with Updates (1)', exact: true }));
  expect(calls.filter(c => c.body?.action === 'update')).toHaveLength(0);
  expect(screen.getByRole('dialog').textContent).toContain('Office / media');
  expect(screen.getByRole('dialog').textContent).not.toContain('Office / books');
  const input = screen.getByRole('dialog').querySelector('input');
  await fireEvent.input(input, { target: { value: 'UPDATE SELECTED' } });
  await fireEvent.submit(input.closest('form'));
  await waitFor(() => expect(calls.some(c => c.body?.action === 'update')).toBe(true));
  const request = calls.find(c => c.body?.action === 'update').body;
  expect(request.stack_ids).toEqual(['media']);
  expect(request.only_available).toBe(true);
  expect(request.confirm).toBe('UPDATE SELECTED');
  expect(screen.getByRole('heading', { name: 'Overview', exact: true })).toBeTruthy();
});

test('a fresh password login lands on Overview', async () => {
  const handler=globalThis.fetch;
  vi.stubGlobal('fetch', vi.fn(async (url,options) => url === '/api/session'
    ? { ok:false,status:401,text:async()=>JSON.stringify({error:'Sign in'}) }
    : handler(url,options)));
  render(App);
  const input=await screen.findByLabelText('Administrator password');
  await fireEvent.input(input,{target:{value:'test-only-password-123'}});
  await fireEvent.submit(input.closest('form'));
  await screen.findByRole('heading',{name:'Overview',exact:true});
  expect(calls.some(call=>call.url==='/api/login')).toBe(true);
});
