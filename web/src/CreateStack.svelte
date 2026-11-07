<script>
 export let hosts,api,changed;
 let open=false,pending=false,error='',confirmation='',content='',form;
 function dialog(node){node.showModal();return{destroy(){node.close();}};}
 function begin(){form={id:'',host_id:hosts[0]?.id||'',name:'',directory:'',project:'',files:[]};confirmation='';content='';error='';open=true;}
 async function create(event){event.preventDefault();pending=true;error='';try{await api('/stacks/create',{method:'POST',body:JSON.stringify({stack:form,content,confirm:confirmation})});await changed();open=false;}catch(e){error=e.message;}finally{pending=false;}}
</script>
<button onclick={begin} disabled={!hosts.length}>+ Create stack</button>
{#if open}<dialog class="modal wide-modal" use:dialog aria-label="Create Compose stack" oncancel={()=>open=false} onclose={()=>open=false}><button class="modal-close" aria-label="Close stack creation" onclick={()=>open=false}>×</button><h2>Create a Compose stack</h2><p>Choose a new directory. Dockmin validates and writes compose.yaml, then registers it. Deploy from the stack card when ready.</p><form onsubmit={create}>
<label>Host<select bind:value={form.host_id}>{#each hosts as host}<option value={host.id}>{host.name}</option>{/each}</select></label><label>Stack name<input bind:value={form.name} required maxlength="100"/></label><label>New absolute directory<input bind:value={form.directory} required placeholder="/home/jim/new-stack"/></label><label>Project name (optional)<input bind:value={form.project}/></label><label>Compose configuration<textarea class="compose-input" bind:value={content} rows="16" spellcheck="false" required></textarea></label><label>Type {form.name||'the stack name'} to create<input bind:value={confirmation} autocomplete="off"/></label>{#if error}<p class="error" role="alert">{error}</p>{/if}<button class="primary" disabled={pending||!form.name||confirmation!==form.name}>{pending?'Validating…':'Create and register'}</button></form></dialog>{/if}
