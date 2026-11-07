<script>
  export let host, api, stacks, changed;
  let mode = '', pending = false, error = '', notice = '', candidates = [], selected = [], standalone = [];
  let scan = null, identity = null, chosen = '', confirmation = '', connection=null,remotePassword='',remotePrivateKey='',authMethod='password',bootstrapConfirmation='';
  function dialog(node) { node.showModal(); return { destroy(){node.close();} }; }
  async function run(task) { pending = true; error = ''; try { await task(); } catch(e){error=e.message;} finally{pending=false;} }
  function registered(stack) { return stacks.some(s => s.host_id === host.id && s.directory === stack.directory && s.project === stack.project && JSON.stringify(s.files) === JSON.stringify(stack.files)); }
  async function discover() { mode='discovery'; notice=''; candidates=[]; selected=[]; standalone=[]; await run(async () => {const result=await api(`/hosts/${host.id}/discover`,{method:'POST',body:'{}'});candidates=result.stacks;standalone=result.standalone;selected=candidates.map((_,i)=>i).filter(i=>!registered(candidates[i]));}); }
  async function register(event) { event.preventDefault(); await run(async () => {
    let n=0;
    try { for(const index of selected) {
      if(!registered(candidates[index])) { await api('/stacks',{method:'POST',body:JSON.stringify(candidates[index])}); n++; }
      selected=selected.filter(i=>i!==index);
    } } finally { await changed(); }
    notice=`Registered ${n} stacks.`;
  }); }
  async function ssh() {mode='ssh';scan=null;identity=null;chosen='';confirmation='';notice='';await run(async()=>{connection=(await api(`/hosts/${host.id}/connection`)).connection;identity=await api(`/hosts/${host.id}/identity`);scan=await api(`/hosts/${host.id}/keys`,{method:'POST',body:'{}'});});}
  async function trust(event){event.preventDefault();await run(async()=>{const key=scan.keys.find(k=>k.fingerprint===chosen);if(!key)throw new Error('Choose a verified key.');await api(`/hosts/${host.id}/trust`,{method:'POST',body:JSON.stringify({...key,confirm:confirmation})});scan=await api(`/hosts/${host.id}/keys`,{method:'POST',body:'{}'});notice='Verified host key enrolled. Test the connection next.';});}
  async function authorize(event){event.preventDefault();const password=remotePassword,private_key=remotePrivateKey;remotePassword='';remotePrivateKey='';await run(async()=>{const result=await api(`/hosts/${host.id}/bootstrap`,{method:'POST',body:JSON.stringify({password,private_key,confirm:bootstrapConfirmation})});notice=result.docker_ready?'SSH key authorized. Docker and Compose are ready.':`SSH key authorized. Docker still needs attention: ${result.detail}`;bootstrapConfirmation='';});}
  async function saveConnection(event){event.preventDefault();await run(async()=>{await api(`/hosts/${host.id}/connection`,{method:'POST',body:JSON.stringify({...connection,name:host.name})});scan=null;identity=null;confirmation='';notice='Connection saved. Reopen SSH setup to verify this destination’s fingerprint.';await changed();});}
</script>
<button disabled={pending} onclick={discover}>Discover stacks</button><button disabled={pending} onclick={ssh}>SSH setup</button>
{#if mode}<dialog class="modal host-tools" use:dialog aria-labelledby={`tools-${host.id}`} oncancel={() => mode=''} onclose={() => mode=''}>
  <button class="modal-close" onclick={() => mode=''} aria-label="Close host setup">×</button><h2 id={`tools-${host.id}`}>{mode==='discovery'?'Discover Compose stacks':'SSH setup'} · {host.name}</h2>
  {#if error}<p class="error" role="alert">{error}</p>{/if}{#if notice}<p class="notice" role="status">{notice}</p>{/if}{#if pending}<p>Working…</p>{/if}
  {#if mode==='discovery'}<p>Find existing stacks using Docker Compose labels, including stopped containers.</p>
    <form onsubmit={register}>{#each candidates as candidate,i}<label class="discovered-stack"><input type="checkbox" checked={selected.includes(i)} disabled={registered(candidate)||pending} onchange={() => selected=selected.includes(i)?selected.filter(n=>n!==i):[...selected,i]} /><span><strong>{candidate.name}{registered(candidate)?' · Registered':''}</strong><code>{candidate.directory}</code><small>{candidate.files.join(', ')||'Compose defaults'}</small></span></label>{/each}
      {#if !pending && !candidates.length && !error}<p>No Compose stacks found. You can register a directory manually.</p>{/if}
      {#if standalone.length}<p>{standalone.length} containers without usable Compose labels: {standalone.join(', ')}.</p>{/if}
      <button class="primary" disabled={pending||!selected.length}>Register selected stacks ({selected.length})</button>
    </form>
  {:else}
    {#if connection}<details><summary>Connection settings</summary><form onsubmit={saveConnection}><label>Hostname or IP<input bind:value={connection.address} required/></label><label>SSH user<input bind:value={connection.user} required/></label><label>Port<input type="number" min="1" max="65535" bind:value={connection.port} required/></label><button disabled={pending}>Save connection</button></form></details>{/if}
    {#if identity}<p>Connect as <strong>{identity.user}</strong> to <code>{identity.hostname}:{identity.port}</code>.</p>
      {#each identity.public_keys as key}<p>Add this public key to the remote account’s <code>~/.ssh/authorized_keys</code> when configuring a new host:</p><textarea class="public-key" readonly value={key} rows="3" aria-label="Service public key"></textarea>{/each}
      {#if !identity.public_keys.length}<p>No explicit service public key was found. Configure the service SSH identity on Pi2.</p>{/if}
    {/if}
    {#if scan}<p>Verify fingerprints directly on the remote machine with <code>ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub</code> (or the matching key file).</p>
      <form onsubmit={trust}>{#each scan.keys as key}<label class="key-candidate"><input type="radio" name={`key-${host.id}`} value={key.fingerprint} bind:group={chosen} disabled={key.trusted||scan.has_trusted_keys} /><span><strong>{key.algorithm} · {key.trusted?'Trusted':'Unverified'}</strong><code>{key.fingerprint}</code></span></label>{/each}
        {#if scan.has_trusted_keys}<small>Existing trust is preserved. A changed host key must be verified and replaced manually on Pi2.</small>
        {:else}<label>After verifying the fingerprint, type {host.alias}<input bind:value={confirmation} autocomplete="off" /></label><button class="primary" disabled={pending||!chosen||confirmation!==host.alias}>Enroll verified key</button>{/if}
      </form>
    {/if}
    {#if scan?.keys.some(k=>k.trusted)}<h3>Authorize Dockmin on this host</h3><p>Authorize the service key using the remote account’s password or an existing private key. These credentials are used once and discarded.</p><form onsubmit={authorize}><label>Authentication method<select bind:value={authMethod} onchange={()=>{remotePassword='';remotePrivateKey='';}}><option value="password">SSH password</option><option value="key">One-time private key</option></select></label>{#if authMethod==='key'}<label>One-time SSH private key<textarea bind:value={remotePrivateKey} rows="5" spellcheck="false" required></textarea></label><label>Key passphrase (if encrypted)<input type="password" bind:value={remotePassword} autocomplete="off"/></label>{:else}<label>Remote SSH account password<input type="password" bind:value={remotePassword} autocomplete="off" required/></label>{/if}<label>Type {host.alias}<input bind:value={bootstrapConfirmation} autocomplete="off"/></label><button disabled={pending||bootstrapConfirmation!==host.alias}>Authorize service key</button></form>{/if}
  {/if}
</dialog>{/if}
