<script>
 export let stack,api,onjob;
 let mode='',pending=false,error='',mounts=[],selected=[],backups=[],checkpoints=[],policy={auto_rollback:false,health_timeout:120},task=null,confirmation='',restoreConfiguration=false;
 function dialog(node){node.showModal();return{destroy(){node.close();}};}
 async function load(which){mode=which;task=null;error='';pending=true;try{
  if(which==='data'){const[data,saved]=await Promise.all([api(`/stacks/${stack.id}/data`),api(`/stacks/${stack.id}/data-backups`)]);mounts=data.mounts;backups=saved.backups;selected=mounts.filter(m=>!m.read_only&&!m.shared_with.length).map(m=>m.id);}
  else{const result=await api(`/stacks/${stack.id}/checkpoints`);checkpoints=result.checkpoints;policy=result.policy;}
 }catch(e){error=e.message;}finally{pending=false;}}
 function ask(action,point=null){confirmation='';task={action,point,expected:`${action==='backup'?'BACKUP':action==='restore'?'RESTORE':action==='deploy'?'DEPLOY':'ROLLBACK'} ${stack.name}`};}
 async function run(event){event.preventDefault();pending=true;error='';try{let path,body;
  if(task.action==='backup'){path=`/stacks/${stack.id}/data-backups`;body={mount_ids:selected,confirm:confirmation};}
  else if(task.action==='restore'){path=`/stacks/${stack.id}/restore`;body={backup:task.point.id,confirm:confirmation,configuration:restoreConfiguration};}
  else{path=`/stacks/${stack.id}/${task.action}`;body={checkpoint:task.point?.id||'',confirm:confirmation};}
  const result=await api(path,{method:'POST',body:JSON.stringify(body)});await onjob(result.job.id);mode='';
 }catch(e){error=e.message;}finally{pending=false;}}
 async function savePolicy(event){event.preventDefault();pending=true;error='';try{await api(`/stacks/${stack.id}/deployment-policy`,{method:'POST',body:JSON.stringify(policy)});}catch(e){error=e.message;}finally{pending=false;}}
 async function importBackup(event){const file=event.target.files[0];if(!file)return;if(file.size>100*1024*1024*1024){error='Upload a backup smaller than 100 GiB.';return;}pending=true;error='';try{await api(`/stacks/${stack.id}/data-backups/import`,{method:'POST',headers:{'Content-Type':'application/x-tar'},body:file});backups=(await api(`/stacks/${stack.id}/data-backups`)).backups;}catch(e){error=e.message;}finally{pending=false;event.target.value='';}}
 function size(n){return n>1073741824?`${(n/1073741824).toFixed(1)} GiB`:`${(n/1048576).toFixed(1)} MiB`;}
</script>
<button onclick={()=>load('data')}>Data backups</button><button onclick={()=>load('recovery')}>Deploy / rollback</button>
{#if mode}<dialog class="modal wide-modal" use:dialog aria-label={`${mode==='data'?'Data backups':'Deploy and rollback'} · ${stack.name}`} oncancel={()=>mode=''} onclose={()=>mode=''}><button class="modal-close" aria-label="Close stack tools" onclick={()=>mode=''}>×</button><h2>{mode==='data'?'Data backups':'Deploy and rollback'} · {stack.name}</h2>
{#if pending}<p>Working…</p>{/if}{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if !task}
{#if mode==='data'}<p>Choose the mounts to archive. Dockmin stops this stack for consistency, then restarts services that were running. The first backup may download the Debian archive helper image.</p>
{#each mounts as mount}<label class="discovered-stack"><input type="checkbox" checked={selected.includes(mount.id)} disabled={mount.shared_with.length>0} onchange={()=>selected=selected.includes(mount.id)?selected.filter(id=>id!==mount.id):[...selected,mount.id]}/><span><strong>{mount.source}</strong><small>{mount.type} · {mount.targets.join(', ')}{mount.read_only?' · Read-only':''}</small>{#if mount.shared_with.length}<small class="error">Also used by running containers: {mount.shared_with.join(', ')}</small>{/if}</span></label>{/each}
{#if !mounts.length&&!pending}<p>No supported data mounts found. Deploy or start the stack once so Docker can report its mounts.</p>{/if}
<button class="primary" onclick={()=>ask('backup')} disabled={pending||!selected.length}>Back up selected data</button><h3>Saved backups</h3><label>Import a downloaded backup (.tar, up to 100 GiB)<input type="file" accept=".tar,application/x-tar" onchange={importBackup} disabled={pending}/></label><small>Import verifies files and matching mount paths. It does not restore or change application data.</small>
{#each backups as backup}<article class="backup-row"><div><strong>{new Date(backup.created*1000).toLocaleString()}</strong><small>{backup.mounts} mounts · {size(backup.bytes)}</small><small>{backup.sources.join(', ')}</small></div><a class="button-link" href={`/api/stacks/${stack.id}/data-backups/${backup.id}/download`} download>Download</a><button onclick={()=>ask('restore',backup)}>Restore</button></article>{/each}
{:else}<p>Deploy saved Compose files and wait for container health. Each deployment keeps the previous images and configuration as a checkpoint. Checkpoints and data backups stay on the managed host.</p><button class="primary" onclick={()=>ask('deploy')}>Deploy saved configuration</button>
<form onsubmit={savePolicy}><label class="inline-option"><input type="checkbox" bind:checked={policy.auto_rollback}/> Automatically restore the checkpoint if deployment health fails</label><label>Health deadline (seconds)<input type="number" min="10" max="600" bind:value={policy.health_timeout}/></label><button disabled={pending}>Save deployment policy</button></form><p>Image/configuration rollback preserves application data. Use a matching data backup when you need to undo a database migration.</p><h3>Deployment checkpoints</h3>
{#each checkpoints as point}<article class="backup-row"><div><strong>{new Date(point.created*1000).toLocaleString()}</strong><small>{Object.keys(point.images).join(', ')||'Configuration only'}</small></div><button disabled={!Object.keys(point.images).length} onclick={()=>ask('rollback',point)}>Roll back</button></article>{/each}
{/if}
{:else}<h3>{task.action} · {stack.name}</h3><p>{task.action==='restore'?'Restoring replaces the saved mounts’ current contents. Dockmin takes a safety backup first and verifies archive checksums. A failed partial restore leaves services stopped.':task.action==='backup'?'This temporarily stops the stack while selected data is archived. Large media directories can take considerable time and disk space.':task.action==='rollback'?'Restore the checkpoint’s images and Compose configuration. This interrupts services and does not undo changes to application data.':'Apply the saved Compose configuration. Services may restart while Dockmin waits for health.'}</p>
{#if task.action==='restore'}<p>Targets: {task.point.sources.join(', ')}</p><label class="inline-option"><input type="checkbox" bind:checked={restoreConfiguration}/> Also restore the matching images and Compose configuration</label>{/if}
<form onsubmit={run}><label>Type {task.expected}<input bind:value={confirmation} autocomplete="off"/></label><button class="primary" disabled={pending||confirmation!==task.expected}>Confirm {task.action}</button><button type="button" onclick={()=>task=null} disabled={pending}>Back</button></form>
{/if}</dialog>{/if}
