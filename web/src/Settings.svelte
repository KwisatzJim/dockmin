<script>
  import Schedule from './Schedule.svelte';
  export let stacks=[];
  export let api, csrf, preferences, changed, passwordChanged;
  let form = { ...preferences }, pending = false, error = '', notice = '';
  let currentPassword = '', newPassword = '', repeatPassword = '', backupPassword = '';
  let scheduleRevision=0;
  let configuration = null, fileName = '', importConfirmation = '', applySettings = false;
  async function run(task) { pending = true; error = ''; notice = ''; try { await task(); } catch(e) { error = e.message; } finally { pending = false; } }
  async function save(event) { event.preventDefault(); await run(async () => { await api('/settings', { method:'POST', body:JSON.stringify(form) }); await changed(); notice = 'Settings saved.'; }); }
  async function password(event) { event.preventDefault(); await run(async () => {
    if(newPassword !== repeatPassword) throw new Error('New passwords do not match.');
    const result = await api('/password', { method:'POST', body:JSON.stringify({ current_password:currentPassword, new_password:newPassword }) });
    passwordChanged(result.csrf); currentPassword = newPassword = repeatPassword = ''; notice = 'Password changed. Other sessions have been signed out.';
  }); }
  async function download(path, name, body) {
    const response = await fetch(`/api${path}`, { method:body ? 'POST' : 'GET', headers:{ 'Content-Type':'application/json', 'X-CSRF-Token':csrf }, ...(body ? {body:JSON.stringify(body)} : {}) });
    if(!response.ok) { const result = await response.json(); throw new Error(result.error || 'Download failed'); }
    const url = URL.createObjectURL(await response.blob()); const link = document.createElement('a'); link.href = url; link.download = name; link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  async function loadFile(event) { configuration = null; importConfirmation = ''; await run(async () => {
    const file = event.target.files[0]; if(!file) return; if(file.size > 1024*1024) throw new Error('Configuration file must be smaller than 1 MiB.');
    const value = JSON.parse(await file.text());
    if(!Array.isArray(value) && !(value.format === 'dockmin' && [1,2].includes(value.version) && Array.isArray(value.hosts) && Array.isArray(value.stacks))) throw new Error('Choose a Dockmin export or Dockman hosts.json file.');
    configuration = value; fileName = file.name;
  }); }
  $: incomingHosts = configuration ? (Array.isArray(configuration) ? configuration.length : configuration.hosts.length) : 0;
  $: incomingStacks = configuration ? (Array.isArray(configuration) ? configuration.reduce((n,h) => n + (h.compose_dirs?.length || 0),0) : configuration.stacks.length) : 0;
  async function importFile(event) { event.preventDefault(); await run(async () => {
    const result = await api('/config/import', { method:'POST', body:JSON.stringify({configuration,confirm:importConfirmation,apply_settings:applySettings && !Array.isArray(configuration)}) });
    if (applySettings && !Array.isArray(configuration)) form = {...configuration.settings};
    configuration = null; importConfirmation = ''; await changed(); scheduleRevision++; notice = `Imported ${result.hosts_added} new hosts and ${result.stacks_added} new stacks.`;
  }); }
</script>
<div class="settings-grid">
  {#key scheduleRevision}<Schedule {api} {stacks}/>{/key}
  {#if error}<p class="error banner" role="alert">{error}</p>{/if}{#if notice}<p class="notice banner" role="status">{notice}</p>{/if}
  <section class="settings-card"><h2>Refresh and operations</h2><p>Health checks retrieve current container status. Image updates remain manual.</p><form onsubmit={save}>
    <label>Health refresh interval (seconds, 0 to disable)<input type="number" min="0" max="3600" bind:value={form.health_refresh_seconds} required /></label>
    <label>Browser refresh interval (seconds)<input type="number" min="2" max="60" bind:value={form.browser_poll_seconds} required /></label>
    <label>Concurrent operations across hosts<input type="number" min="1" max="16" bind:value={form.concurrency} required /></label>
    <label>Operation timeout (seconds)<input type="number" min="10" max="7200" bind:value={form.command_timeout_seconds} required /></label>
    <label>Completed operations to retain<input type="number" min="20" max="1000" bind:value={form.history_limit} required /></label>
    <small>Unreachable hosts are checked less often. Each host runs one operation at a time. Reducing history can remove older image-check results; run a fresh check when needed.</small><button class="primary" disabled={pending}>Save settings</button>
  </form></section>
  <section class="settings-card"><h2>Administrator password</h2><form onsubmit={password}>
    <label>Current password<input type="password" bind:value={currentPassword} autocomplete="current-password" required /></label>
    <label>New password<input type="password" bind:value={newPassword} autocomplete="new-password" minlength="12" required /></label>
    <label>Repeat new password<input type="password" bind:value={repeatPassword} autocomplete="new-password" minlength="12" required /></label>
    <small>Changing the password revokes other sessions. This session stays signed in.</small><button disabled={pending}>Change password</button>
  </form></section>
  <section class="settings-card"><h2>Configuration</h2><p>Export host aliases, registered stacks, and settings. SSH credentials and passwords are excluded.</p>
    <button disabled={pending} onclick={() => run(() => download('/config/export','dockmin-config.json'))}>Export configuration</button>
    <form onsubmit={importFile}><label>Import Dockmin configuration or Dockman hosts.json<input type="file" accept=".json,application/json" onchange={loadFile} disabled={pending} /></label>
      {#if configuration}<p>{fileName}: {incomingHosts} hosts and {incomingStacks} stacks. Existing aliases and identical stacks are skipped; existing configuration is preserved.</p>
        {#if !Array.isArray(configuration)}<label class="inline-option"><input type="checkbox" bind:checked={applySettings} /> Apply imported settings</label>{/if}
        <label>Type IMPORT CONFIGURATION to merge<input bind:value={importConfirmation} autocomplete="off" /></label><button disabled={pending || importConfirmation !== 'IMPORT CONFIGURATION'}>Import configuration</button>
      {/if}
    </form>
  </section>
  <section class="settings-card"><h2>Database backup</h2><p>Download configuration, password hash, saved ntfy token, cached status, and operation history. Sessions and SSH private keys are excluded. Keep this file private; it can contain credentials and application output.</p>
    <form onsubmit={event => { event.preventDefault(); run(async () => { const value = backupPassword; backupPassword = ''; await download('/backups','dockmin-backup.sqlite',{password:value}); notice = 'Database backup downloaded.'; }); }}>
      <label>Password to authorize backup<input type="password" bind:value={backupPassword} autocomplete="current-password" required /></label><button disabled={pending}>Download database backup</button>
    </form><small>Restore on Pi2 while the service is stopped, following the backup guide supplied with Dockmin.</small>
  </section>
</div>
