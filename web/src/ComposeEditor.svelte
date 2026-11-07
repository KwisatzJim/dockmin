<script>
  export let stack,api,onjob;
  let open=false,files=[],preview=null,pending=false,error='',confirmation='';
  function dialog(node){node.showModal();return{destroy(){node.close();}};}
  async function load(){open=true;files=[];preview=null;confirmation='';pending=true;error='';try{files=(await api(`/stacks/${stack.id}/compose`)).files;}catch(e){error=e.message;}finally{pending=false;}}
  async function validate(){pending=true;error='';try{preview=await api(`/stacks/${stack.id}/compose/preview`,{method:'POST',body:JSON.stringify({files})});if(!preview.valid)error=preview.error;}catch(e){error=e.message;}finally{pending=false;}}
  async function save(event){event.preventDefault();pending=true;error='';try{const result=await api(`/stacks/${stack.id}/compose`,{method:'POST',body:JSON.stringify({files,confirm:confirmation})});await onjob(result.job.id);open=false;}catch(e){error=e.message;}finally{pending=false;}}
</script>
<button onclick={load}>Edit Compose</button>
{#if open}<dialog class="modal wide-modal" use:dialog oncancel={()=>open=false} onclose={()=>open=false} aria-label={`Compose editor · ${stack.name}`}><button class="modal-close" aria-label="Close Compose editor" onclick={()=>open=false}>×</button><h2>Edit Compose · {stack.name}</h2><p>Validate and review changes before saving. Previous files are backed up on the host. Saving does not recreate containers.</p>
<button onclick={load} disabled={pending}>Reload files</button>{#if pending}<p>Working…</p>{/if}{#if error}<p class="error" role="alert">{error}</p>{/if}
<form onsubmit={save}>{#each files as file}<label>{file.path}<textarea class="compose-input" rows="16" bind:value={file.content} oninput={()=>preview=null} spellcheck="false" disabled={pending}></textarea></label>{/each}
<button type="button" onclick={validate} disabled={pending||!files.length}>Validate and preview</button>
{#if preview?.valid}<p class="notice" role="status">Compose configuration is valid.</p><pre class="diff-output">{preview.diff||'No changes.'}</pre><label>Type {stack.name} to save<input bind:value={confirmation} autocomplete="off"/></label><button class="primary" disabled={pending||confirmation!==stack.name}>Save Compose files</button>{/if}
</form></dialog>{/if}
