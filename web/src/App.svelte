<script>
  import { onMount } from 'svelte';
  import ContainerTerminal from './Terminal.svelte';
  import Containers from './Containers.svelte';
  import StackTools from './StackTools.svelte';
  import ComposeEditor from './ComposeEditor.svelte';
  import CreateStack from './CreateStack.svelte';
  import Settings from './Settings.svelte';
  import HostTools from './HostTools.svelte';
  import LiveLogs from './LiveLogs.svelte';
  let signedIn = false, ready = false, password = '', csrf = '', error = '', notice = '', pending = false;
  let hosts = [], stacks = [], jobs = [], statuses = {}, updates = {}, tab = 'overview', filter = 'all', selected = [];
  let modal = null, activeJob = null, stream = null, confirmation = '', timer;
  let hostForm = { id: '', name: '', alias: '' }, newConnection=false, connectionForm={address:'',user:'jim',port:22};
  let stackForm = { id: '', host_id: '', name: '', directory: '', project: '', files: [] }, filesText = '';
  let notifications = [], seenNotifications = null;
  let confirmTask = null, logStack = null, aliases = [], hostHealth = {};
  let preferences = {health_refresh_seconds:60,browser_poll_seconds:5,concurrency:3,command_timeout_seconds:1800,history_limit:100};
  function modalDialog(node) { node.showModal(); return { destroy() { node.close(); } }; }
  async function api(path, options = {}) {
    const r = await fetch(`/api${path}`, { ...options, headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf, ...options.headers } });
    const text = await r.text(); let data;
    try { data = JSON.parse(text); } catch { data = { error: text }; }
    if (!r.ok) { if (r.status === 401 && path !== '/login') { signedIn = false; stream?.close(); logStack = null; modal = null; terminalTarget=null; } throw new Error(data.error || `Request failed (${r.status})`); }
    return data;
  }
  async function refresh() {
    const data = await api('/overview'); hosts = data.hosts; stacks = data.stacks; jobs = data.jobs; statuses = data.statuses; updates = data.updates || {}; preferences = data.settings || preferences; hostHealth = data.host_health || {};
    notifications = data.notifications || [];
    if (seenNotifications && localStorage.getItem('dockmin-browser-notifications') === 'true' && 'Notification' in window && Notification.permission === 'granted') { for (const item of notifications) if (!item.read && !seenNotifications.has(item.id)) new Notification(item.title,{body:item.body.slice(0,200)}); }
    seenNotifications = new Set(notifications.map(n=>n.id));
    selected = selected.filter(id => stacks.some(s => s.id === id));
  }
  async function attempt(task) {
    pending = true; error = ''; notice = '';
    try { await task(); } catch (e) { error = e.message; } finally { pending = false; }
  }
  async function login(event) {
    event.preventDefault(); await attempt(async () => { const s = await api('/login', { method: 'POST', body: JSON.stringify({ password }) }); csrf = s.csrf; password = ''; tab = 'overview'; signedIn = true; await refresh(); });
  }
  async function logout() { await attempt(async () => { await api('/logout', { method: 'POST', body: '{}' }); signedIn = false; stream?.close(); logStack = null; modal = null; terminalTarget=null; activeJob = null; }); }
  onMount(() => {
    (async () => { try { const s = await api('/session'); csrf = s.csrf; signedIn = true; await refresh(); } catch (e) { if (signedIn) error = e.message; } finally { ready = true; } })();
    let stopped = false;
    async function poll() { if (signedIn) await refresh().catch(e => error = e.message); if (!stopped) timer = setTimeout(poll, preferences.browser_poll_seconds * 1000); }
    timer = setTimeout(poll, preferences.browser_poll_seconds * 1000);
    return () => { stopped = true; clearTimeout(timer); stream?.close(); };
  });
  function hostName(id) { return hosts.find(h => h.id === id)?.name || 'Removed host'; }
  function targetName(job) { return stacks.find(s => s.id === job.stack_id)?.name || hostName(job.host_id); }
  function serviceState(s) { return s.State || s.state || 'unknown'; }
  function stackState(stack) {
    const status = statuses[stack.id];
    if (hostHealth[stack.host_id]?.state === 'offline') return 'unknown';
    if (!status || status.error) return 'unknown';
    if (!status.services.length) return 'stopped';
    if (status.services.every(s => serviceState(s) === 'running' && !['unhealthy', 'starting'].includes(s.Health))) return 'running';
    if (status.services.every(s => ['exited', 'created'].includes(serviceState(s)))) return 'stopped';
    return 'attention';
  }
  function age(t) { if (!t) return 'Never checked'; const n = Math.max(0, Math.floor(Date.now()/1000-t)); return n < 60 ? `${n}s ago` : n < 3600 ? `${Math.floor(n/60)}m ago` : `${Math.floor(n/3600)}h ago`; }
  function portText(s) { return s.Ports || (s.Publishers || []).map(p => `${p.PublishedPort} → ${p.TargetPort}/${p.Protocol}`).join(', ') || '—'; }
  $: visible = stacks.filter(s => filter === 'all' || s.host_id === filter);
  $: running = stacks.filter(s => stackState(s) === 'running').length;
  $: active = jobs.filter(j => !j.finished).length;
  $: available = stacks.filter(s => updates[s.id]?.state === 'available');
  $: checking = stacks.some(s => ['queued', 'checking'].includes(updates[s.id]?.state));
  $: updating = stacks.some(s => updates[s.id]?.state === 'updating');
  function updateLabel(state) { return ({ unchecked: 'Not checked', queued: 'Check queued', checking: 'Checking…', available: 'Update available', current: 'Up to date', unknown: 'Unknown', failed: 'Check failed', updating: 'Updating…' })[state] || 'Not checked'; }
  function serviceSummary(stack) { const services = statuses[stack.id]?.services || []; return `${services.filter(s => serviceState(s) === 'running').length}/${services.length} services running`; }

  function addHost(h) { newConnection=false; connectionForm={address:'',user:'jim',port:22}; api('/ssh/aliases').then(data => aliases = data.aliases).catch(e => error = e.message); hostForm = h ? { ...h } : { id: '', name: '', alias: '' }; modal = 'host'; }
  function addStack(s) { stackForm = s ? { ...s } : { id: '', host_id: filter === 'all' ? (hosts[0]?.id || '') : filter, name: '', directory: '', project: '', files: [] }; filesText = stackForm.files.join('\n'); modal = 'stack'; }
  async function saveHost(event) { event.preventDefault(); await attempt(async () => { await api(newConnection?'/hosts/new':'/hosts', { method: 'POST', body: JSON.stringify(newConnection?{name:hostForm.name,alias:hostForm.alias,...connectionForm}:hostForm) }); modal = null; await refresh(); notice = newConnection?'Host added. Open SSH setup to verify its fingerprint and authorize the service key.':'Host saved. Test its SSH connection next.'; }); }
  async function saveStack(event) { event.preventDefault(); await attempt(async () => { await api('/stacks', { method: 'POST', body: JSON.stringify({ ...stackForm, files: filesText.split('\n').map(s => s.trim()).filter(Boolean) }) }); modal = null; await refresh(); notice = 'Stack registered. Refresh to retrieve its services.'; }); }
  function ask(title, detail, expected, run) { confirmTask = { title, detail, expected, run }; confirmation = ''; modal = 'confirm'; }
  async function accept(event) { event.preventDefault(); if (confirmation !== confirmTask.expected) return; await attempt(async () => { await confirmTask.run(); modal = null; }); }
  async function openJob(id) {
    logStack = null; stream?.close(); activeJob = await api(`/jobs/${id}`);
    if (activeJob.finished) return;
    stream = new EventSource(`/api/jobs/${id}/events`);
    stream.addEventListener('job', e => { activeJob = JSON.parse(e.data); if (activeJob.finished) { stream.close(); refresh().catch(e => error = e.message); } });
    stream.onerror = () => { if (activeJob?.finished) stream.close(); };
  }
  async function sendAction(kind, item, action, confirm = '') {
    const job = await api(`/${kind}/${item.id}/actions`, { method: 'POST', body: JSON.stringify({ action, confirm }) }); await openJob(job.id); await refresh();
  }
  function action(kind, item, name) {
    if (['stop', 'restart', 'update', 'prune-images', 'prune-containers', 'prune-build'].includes(name)) {
      const detail = name.startsWith('prune') ? 'This deletes unused Docker resources across the entire host. Pruning stopped containers also removes retained standalone rollback versions and their writable-layer data.' : name === 'update' ? 'Pull images and recreate this Compose stack. Services may be briefly unavailable.' : 'This may interrupt services in the selected stack.';
      ask(`${name.replaceAll('-', ' ')} · ${item.name}`, detail, item.name, () => sendAction(kind, item, name, item.name));
    } else attempt(() => sendAction(kind, item, name));
  }
  function toggle(id) { selected = selected.includes(id) ? selected.filter(x => x !== id) : [...selected, id]; }
  async function bulk(action, ids, onlyAvailable = false) {
    const stayOnOverview = tab === 'overview';
    const run = async () => { const result = await api('/bulk', { method: 'POST', body: JSON.stringify({ stack_ids: ids, action, only_available: onlyAvailable, confirm: action === 'update' ? 'UPDATE SELECTED' : '' }) }); await refresh(); if (action === 'check-updates' && !stayOnOverview) tab = 'operations'; if (result.jobs.length && !stayOnOverview) await openJob(result.jobs[0].id); notice = action === 'check-updates' ? `Checking ${result.jobs.length} stacks for image updates. Results update here as each check finishes.` : `Queued ${result.jobs.length} ${action} operations.`; };
    if (action === 'update') ask(`Update ${ids.length} stacks`, `Targets: ${stacks.filter(s => ids.includes(s.id)).map(s => `${hostName(s.host_id)} / ${s.name}`).join(', ')}. Updates run one at a time on each host.`, 'UPDATE SELECTED', run);
    else await attempt(run);
  }
  function openTerminal(hostId,containerId,name) { stream?.close();activeJob=null;logStack=null;terminalTarget={hostId,containerId,name}; }
  let terminalTarget=null;
  function remove(kind, item) { ask(`Remove ${item.name}`, kind === 'hosts' ? 'Remove this host from Dockmin. Remove its registered stacks first.' : 'Remove this stack from Dockmin’s configuration. Its Docker services and files stay on the host.', item.name, async () => { await api(`/${kind}/${item.id}`, { method: 'DELETE' }); await refresh(); }); }
</script>

{#if !ready}
  <main class="login"><div class="brand"><span class="mark">▦</span> dockmin</div><p>Connecting to your service…</p></main>
{:else if !signedIn}
  <main class="login"><form onsubmit={login} class="login-card">
    <div class="brand"><span class="mark">▦</span> dockmin</div><span class="eyebrow">YOUR FLEET, ONE PLACE</span>
    <h1>Welcome aboard.</h1><p>Sign in to manage your Compose stacks.</p>
    <label>Administrator password<input type="password" bind:value={password} autocomplete="current-password" required /></label>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <button class="primary" disabled={pending}>{pending ? 'Signing in…' : 'Sign in →'}</button>
    <small>Use HTTPS for LAN access or connect through Tailscale.</small>
  </form></main>
{:else}
  <div class="shell">
    <aside class="sidebar">
      <a class="brand" href="/" aria-label="Dockmin home"><span class="mark">▦</span> dockmin</a>
      <span class="eyebrow">WORKSPACE</span>
      <button class:current={tab === 'overview'} onclick={() => tab = 'overview'}>◉ <span>Overview</span></button>
      <button class:current={tab === 'stacks'} onclick={() => tab = 'stacks'}>▦ <span>Compose stacks</span><b>{stacks.length}</b></button>
      <button class:current={tab === 'containers'} onclick={()=>tab='containers'}>▣ <span>Containers</span></button>
      <button class:current={tab === 'hosts'} onclick={() => tab = 'hosts'}>▤ <span>Hosts</span><b>{hosts.length}</b></button>
      <button class:current={tab === 'operations'} onclick={() => tab = 'operations'}>↗ <span>Operations</span>{#if active}<b>{active}</b>{/if}</button>
      <button class:current={tab === 'notifications'} onclick={() => tab = 'notifications'}>♧ <span>Notifications</span>{#if notifications.some(n=>!n.read)}<b>{notifications.filter(n=>!n.read).length}</b>{/if}</button>
      <button class:current={tab === 'settings'} onclick={() => tab = 'settings'}>⚙ <span>Settings</span></button>
      <div class="sidebar-bottom"><span class="status-dot running"></span> Service connected<small>SSH · Compose · Tailscale</small><button onclick={logout}>Sign out</button></div>
    </aside>
    <main class="workspace">
      <header><div><span class="eyebrow">DOCKER ADMINISTRATION</span><h1>{tab === 'overview' ? 'Overview' : tab === 'stacks' ? 'Compose stacks' : tab === 'containers' ? 'Containers' : tab === 'hosts' ? 'Your hosts' : tab === 'notifications' ? 'Notifications' : tab === 'settings' ? 'Settings' : 'Operation history'}</h1><p>{tab === 'overview' ? 'Your stacks, their health, and what needs updating.' : tab === 'stacks' ? 'A clear view of everything you’re running.' : tab === 'containers' ? 'Manage individual containers, logs, and terminals.' : tab === 'notifications' ? 'Update alerts and operation failures, saved while you’re away.' : tab === 'hosts' ? 'Manage your fleet through existing SSH access.' : tab === 'settings' ? 'Preferences, credentials, and configuration backups.' : 'Follow progress, inspect output, and review recent actions.'}</p></div>
        {#if !['overview','settings','notifications','containers'].includes(tab)}<button class="primary" onclick={() => tab === 'hosts' ? addHost() : tab === 'stacks' ? addStack() : attempt(refresh)} disabled={pending || (tab === 'stacks' && !hosts.length)}>{tab === 'hosts' ? '+ Add host' : tab === 'stacks' ? '+ Register stack' : 'Refresh history'}</button>{/if}
      </header>
      {#if error}<div class="error banner" role="alert">{error}<button onclick={() => error = ''} aria-label="Dismiss error">×</button></div>{/if}
      {#if notice}<div class="notice banner" role="status">{notice}<button onclick={() => notice = ''} aria-label="Dismiss notice">×</button></div>{/if}
      <section class="metrics" aria-label="Fleet summary"><div><span>REGISTERED HOSTS</span><strong>{hosts.length}<small>SSH connections</small></strong></div><div><span>COMPOSE STACKS</span><strong>{stacks.length}<small>{running} last seen running</small></strong></div><div><span>ACTIVE OPERATIONS</span><strong>{active}<small>{active ? 'Working in the background' : 'Ready when you are'}</small></strong></div></section>
      {#if tab === 'overview'}
        <div class="fleet-health" aria-label="Host connectivity">{#each hosts as h}<span class={`pill ${hostHealth[h.id]?.state || 'unknown'}`} title={hostHealth[h.id]?.error || ''}>{h.name} · {hostHealth[h.id]?.state || 'Not checked'}</span>{/each}<button disabled={pending} onclick={() => attempt(async () => { await api('/health/refresh',{method:'POST',body:'{}'}); notice='Status refresh requested. Results will appear here.'; })}>Refresh health</button></div>
        <section class="overview-toolbar" aria-label="Overview actions">
          <div><h2>Stack overview</h2><p>{available.length} {available.length === 1 ? 'stack has' : 'stacks have'} updates available{#if checking} · Checking images…{/if}{#if updating} · Updates in progress{/if}</p></div>
          <div class="toolbar-actions"><CreateStack {hosts} {api} changed={refresh}/><button onclick={() => bulk('check-updates', stacks.map(s => s.id))} disabled={pending || checking || updating || !stacks.length}>{checking ? 'Checking for updates…' : 'Check All for Updates'}</button><button class="primary" onclick={() => bulk('update', available.map(s => s.id), true)} disabled={pending || checking || updating || !available.length}>Update All with Updates ({available.length})</button></div>
        </section>
        {#if !stacks.length}<section class="empty"><span class="empty-icon">▦</span><h2>Your fleet at a glance.</h2><p>Add a host and register an existing Compose stack to see its health and available updates here.</p><button class="primary" onclick={() => { if (hosts.length) addStack(); else addHost(); }}>{hosts.length ? '+ Register stack' : '+ Add host'}</button></section>
        {:else}<section class="overview-grid" aria-label="Compose stack summaries">{#each stacks as stack}
          {@const update = updates[stack.id] || { state: 'unchecked' }}
          <article class="overview-card">
            <div class="overview-card-top"><span class="host-tag">{hostName(stack.host_id)}</span><span class={`pill ${stackState(stack)}`}><span class={`status-dot ${stackState(stack)}`}></span>{stackState(stack)}</span></div>
            <h2>{stack.name}</h2><p class="overview-services">{serviceSummary(stack)}</p><p class="overview-names">{(statuses[stack.id]?.services || []).map(s => s.Service || s.Name).join(' · ') || 'Refresh status to retrieve services'}</p>
            {#if statuses[stack.id]?.error}<p class="error">{statuses[stack.id].error}</p>{/if}
            <div class={`overview-update ${update.state}`} role="status"><strong>{updateLabel(update.state)}</strong><small>{update.checked ? `Checked ${age(update.checked)}` : update.state === 'updating' ? 'Images will be checked again after the update' : ['queued', 'checking'].includes(update.state) ? 'Results will appear automatically' : 'Run a check to compare running images with the registry'}</small></div>
            {#if update.output}<details class="overview-result"><summary>Image check details</summary><pre>{update.output}</pre></details>{/if}
            <div class="overview-card-footer"><span>Status {age(statuses[stack.id]?.checked)}{#if statuses[stack.id] && Date.now()/1000 - statuses[stack.id].checked > 60} · stale{/if}</span><button onclick={() => { filter = stack.host_id; tab = 'stacks'; }}>Manage stack →</button></div>
          </article>
        {/each}</section>{/if}
      {:else if tab === 'stacks'}
        <div class="toolbar"><div class="filters"><button class:chosen={filter === 'all'} onclick={() => filter = 'all'}>All hosts</button>{#each hosts as h}<button class:chosen={filter === h.id} onclick={() => filter = h.id}>{h.name}</button>{/each}</div><div class="toolbar-actions"><button onclick={() => bulk('check-updates', stacks.map(s => s.id))} disabled={pending || !stacks.length}>Check all for updates</button><button onclick={() => bulk('refresh', visible.map(s => s.id))} disabled={pending || !visible.length}>↻ Refresh status</button></div></div>
        {#if selected.length}<div class="selection"><span>{selected.length} selected</span><button onclick={() => bulk('update', selected)} disabled={pending}>Update selected</button><button onclick={() => selected = []}>Clear</button></div>{/if}
        {#if !hosts.length}<section class="empty"><span class="empty-icon">▤</span><h2>Start with your first host.</h2><p>Add an SSH alias for one of your Docker PCs, then register its existing Compose directories.</p><button class="primary" onclick={() => { tab = 'hosts'; addHost(); }}>+ Add a host</button></section>
        {:else if !visible.length}<section class="empty"><span class="empty-icon">▦</span><h2>Your stacks belong here.</h2><p>Register an existing Compose directory to see its services and manage it remotely.</p><button class="primary" onclick={() => addStack()}>+ Register stack</button></section>
        {:else}<section class="stack-grid">{#each visible as stack}
          <article class="stack-card"><div class="card-top"><input type="checkbox" checked={selected.includes(stack.id)} onchange={() => toggle(stack.id)} aria-label={`Select ${stack.name}`} /><span class="host-tag">{hostName(stack.host_id)}</span><span class={`pill ${stackState(stack)}`}><span class={`status-dot ${stackState(stack)}`}></span>{stackState(stack)}</span></div>
            <h2>{stack.name}</h2><code class="directory">{stack.directory}</code>
            <div class="services">
              {#if statuses[stack.id]?.error}<p class="error">{statuses[stack.id].error}</p>{/if}
              {#each statuses[stack.id]?.services || [] as service}<div class="service"><span class={`status-dot ${serviceState(service)}`}></span><div><strong>{service.Service || service.Name || 'Service'}</strong><small>{service.Image || 'Image unavailable'}</small><small>{portText(service)}</small></div><span class="service-state">{service.Health || serviceState(service)}</span>{#if service.ID}<button onclick={()=>openTerminal(stack.host_id,service.ID,service.Name)} disabled={serviceState(service)!=='running'}>Terminal</button>{/if}</div>{/each}
              {#if !statuses[stack.id]?.services?.length}<p class="muted">{statuses[stack.id] ? 'No containers reported. The stack may be stopped.' : 'Refresh to retrieve service status.'}</p>{/if}
            </div>
            <div class="card-meta"><span>{age(statuses[stack.id]?.checked)}{#if statuses[stack.id] && Date.now()/1000 - statuses[stack.id].checked > 60} · stale{/if}</span><button class="text-button" onclick={() => addStack(stack)}>Edit</button></div>
            <div class="actions"><button onclick={() => action('stacks', stack, 'start')} disabled={pending}>Start</button><button onclick={() => action('stacks', stack, 'restart')} disabled={pending}>Restart</button><button onclick={() => action('stacks', stack, 'stop')} disabled={pending}>Stop</button><button class="accent" onclick={() => action('stacks', stack, 'update')} disabled={pending}>↑ Update</button></div>
            <div class="secondary-actions"><ComposeEditor {stack} {api} onjob={openJob}/><StackTools {stack} {api} onjob={openJob}/><button onclick={() => { stream?.close(); activeJob = null; logStack = stack; }}>View logs</button><button onclick={() => action('stacks', stack, 'check-updates')}>Check images</button><button class="danger-text" onclick={() => remove('stacks', stack)}>Remove</button></div>
          </article>
        {/each}</section>{/if}
      {:else if tab === 'containers'}
        <Containers {hosts} {api} onjob={openJob} onlogs={item=>{stream?.close();activeJob=null;logStack=item;}} onterminal={(...args)=>openTerminal(...args)}/>
      {:else if tab === 'hosts'}
        {#if !hosts.length}<section class="empty"><h2>A home for your Docker fleet.</h2><p>Configure the service account’s SSH aliases, then add your first host.</p><button class="primary" onclick={() => addHost()}>+ Add host</button></section>{/if}
        <section class="host-list">{#each hosts as host}<article class="host-card"><div class="host-icon">▤</div><div class="host-info"><h2>{host.name}</h2><code>ssh {host.alias}</code><p>{stacks.filter(s => s.host_id === host.id).length} registered stacks · {hostHealth[host.id]?.state || 'Not checked'}</p>{#if hostHealth[host.id]?.checked}<small>Connectivity checked {age(hostHealth[host.id].checked)}</small>{/if}{#if hostHealth[host.id]?.error}<small class="error">{hostHealth[host.id].error}</small>{/if}</div><div class="host-actions"><button onclick={() => action('hosts', host, 'test')} disabled={pending}>Test connection</button><button onclick={() => action('hosts', host, 'disk')} disabled={pending}>Disk usage</button><HostTools {host} {api} {stacks} changed={refresh} /><button onclick={() => addHost(host)}>Edit</button><button onclick={() => { filter = host.id; tab = 'stacks'; }}>View stacks →</button></div><details><summary>Host maintenance</summary><p>Cleanup affects the entire Docker host. Volumes are never deleted.</p><div class="actions"><button onclick={() => action('hosts', host, 'prune-images')}>Prune dangling images</button><button onclick={() => action('hosts', host, 'prune-containers')}>Prune stopped containers</button><button onclick={() => action('hosts', host, 'prune-build')}>Prune build cache</button><button class="danger-text" onclick={() => remove('hosts', host)}>Remove host</button></div></details></article>{/each}</section>
      {:else if tab === 'notifications'}
        <div class="toolbar"><p>Update alerts and operation failures, saved while you’re away.</p><button onclick={()=>attempt(async()=>{await api('/notifications/read',{method:'POST',body:'{}'});await refresh();})}>Mark all read</button></div>
        {#each notifications as item}<article class="settings-card notification"><h2>{item.title}{item.read?'':' · New'}</h2><small>{new Date(item.created*1000).toLocaleString()}</small><pre>{item.body}</pre></article>{/each}{#if !notifications.length}<p>No notifications yet.</p>{/if}
      {:else if tab === 'settings'}
        <Settings {api} {csrf} {preferences} {stacks} changed={refresh} passwordChanged={value => csrf = value} />
      {:else}
        <section class="history"><div class="table-head"><span>Operation / target</span><span>State</span><span>Started</span></div>{#each jobs as job}<button class="history-row" onclick={() => attempt(() => openJob(job.id))}><span><strong>{job.action.replaceAll('-', ' ')}</strong><small>{hostName(job.host_id)} / {targetName(job)}</small>{#if job.result}<small class="check-result">{job.result}</small>{/if}</span><span class={`pill ${job.state}`}>{job.state}</span><time>{new Date(job.created * 1000).toLocaleString()}</time></button>{/each}{#if !jobs.length}<p class="empty">Your operations will appear here.</p>{/if}</section>
      {/if}
      <footer>dockmin <span>Existing stacks. Familiar tools. One place.</span></footer>
    </main>
  </div>
  {#if terminalTarget}<ContainerTerminal target={terminalTarget} {api} close={()=>terminalTarget=null}/>{/if}
  {#if logStack}{#key logStack.id}<LiveLogs stack={logStack} close={() => logStack = null} />{/key}{/if}
  {#if activeJob}<section class="output-panel" aria-label="Operation output"><div class="output-header"><div><strong>{activeJob.action.replaceAll('-', ' ')} · {targetName(activeJob)}</strong><span class={`pill ${activeJob.state}`}>{activeJob.state}</span></div><button onclick={() => { stream?.close(); activeJob = null; }} aria-label="Close output">×</button></div><pre>{activeJob.output || (activeJob.state === 'queued' ? 'Queued — waiting for this host…' : 'Waiting for output…')}</pre><small>Closing this panel does not stop the operation. Logs may contain application-sensitive information.</small></section>{/if}
{/if}

{#if modal}
  <div class="modal-backdrop"><dialog class="modal" aria-labelledby="modal-title" use:modalDialog onclose={() => modal = null} oncancel={() => modal = null}><button class="modal-close" onclick={() => modal = null} aria-label="Close dialog">×</button>
    {#if modal === 'host'}<form onsubmit={saveHost}><span class="eyebrow">FLEET CONFIGURATION</span><h2 id="modal-title">{hostForm.id ? 'Edit host' : 'Add a host'}</h2><p>Register an existing alias or configure a new machine directly.</p>{#if !hostForm.id}<label class="inline-option"><input type="checkbox" bind:checked={newConnection}/> Configure a new SSH connection</label>{/if}<label>Display name<input bind:value={hostForm.name} placeholder="Living room PC" required maxlength="100" /></label><label>SSH alias<input bind:value={hostForm.alias} placeholder="docker-pc1" required maxlength="100" list="ssh-aliases" /><datalist id="ssh-aliases">{#each aliases as alias}<option value={alias}></option>{/each}</datalist></label><div>{#if newConnection}<label>Hostname or IP address<input bind:value={connectionForm.address} required placeholder="100.x.x.x or LAN address"/></label><label>SSH account<input bind:value={connectionForm.user} required/></label><label>SSH port<input type="number" min="1" max="65535" bind:value={connectionForm.port} required/></label>{/if}</div><small>Verify the host fingerprint in SSH setup, then authorize the service key using the remote account’s password.</small><button class="primary" disabled={pending}>Save host</button></form>
    {:else if modal === 'stack'}<form onsubmit={saveStack}><span class="eyebrow">EXISTING COMPOSE STACK</span><h2 id="modal-title">{stackForm.id ? 'Edit stack' : 'Register a stack'}</h2><label>Host<select bind:value={stackForm.host_id} required>{#each hosts as h}<option value={h.id}>{h.name}</option>{/each}</select></label><label>Stack name<input bind:value={stackForm.name} placeholder="Media server" required maxlength="100" /></label><label>Absolute directory on host<input bind:value={stackForm.directory} placeholder="/srv/docker/media" required /></label><label>Compose project name <span>(optional)</span><input bind:value={stackForm.project} placeholder="Use the directory default" /></label><label>Compose files <span>(optional, one per line in order)</span><textarea bind:value={filesText} placeholder="compose.yml&#10;compose.production.yml" rows="2"></textarea></label><small>Leave project and files empty to use Compose defaults. Match any options used when this stack was deployed.</small><button class="primary" disabled={pending}>Save stack</button></form>
    {:else if modal === 'confirm'}<form onsubmit={accept}><span class="eyebrow">CONFIRM OPERATION</span><h2 id="modal-title">{confirmTask.title}</h2><p>{confirmTask.detail}</p><label>Type <strong>{confirmTask.expected}</strong> to continue<input bind:value={confirmation} autocomplete="off" required /></label><button class="primary" disabled={pending || confirmation !== confirmTask.expected}>Confirm</button></form>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </dialog></div>
{/if}
