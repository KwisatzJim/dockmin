<script>
 import {onMount} from 'svelte';
 export let api;
 let form={enabled:false,server:'https://ntfy.sh',topic:''},token='',replaceToken=false,configured=false,deliveries=[],pending=false,error='',notice='';
 async function load(){const data=await api('/ntfy');if(data.preferences){const p=data.preferences;form={enabled:p.enabled,server:p.server||'https://ntfy.sh',topic:p.topic};configured=p.token_configured;deliveries=data.deliveries||[];}}
 onMount(()=>{load().catch(e=>error=e.message);});
 async function save(event){event.preventDefault();pending=true;error='';notice='';try{await api('/ntfy',{method:'POST',body:JSON.stringify({...form,token:replaceToken?token:null})});notice='ntfy settings saved. Future inbox alerts will be delivered when enabled.';await load();}catch(e){error=e.message;}finally{token='';replaceToken=false;pending=false;}}
 async function test(){pending=true;error='';notice='';try{await api('/ntfy/test',{method:'POST',body:'{}'});notice='Test notification sent.';}catch(e){error=e.message;}finally{pending=false;}}
</script>
<section class="settings-card"><h2>ntfy notifications</h2><p>Send update alerts and operation failures to your ntfy topic, including while you’re signed out. Messages contain the alert title; detailed output stays in the inbox.</p>
<form onsubmit={save}><label class="inline-option"><input type="checkbox" bind:checked={form.enabled}/> Enable ntfy</label><label>ntfy server<input type="url" bind:value={form.server} placeholder="https://ntfy.sh" required/></label><label>ntfy topic<input bind:value={form.topic} pattern="[A-Za-z0-9_-]{1,128}" required/></label><small>Subscribe to this topic in ntfy. Use a private, unpredictable topic or an authenticated server. HTTPS protects delivery credentials.</small>
<label class="inline-option"><input type="checkbox" bind:checked={replaceToken}/> {configured?'Replace or remove saved access token':'Set an access token (optional)'}</label>{#if replaceToken}<label>ntfy access token<input type="password" autocomplete="new-password" bind:value={token}/></label><small>Leaving this blank removes the saved token.</small>{/if}<button class="primary" disabled={pending}>Save ntfy settings</button></form><button onclick={test} disabled={pending||!form.enabled}>Send test notification</button><button onclick={()=>load().catch(e=>error=e.message)} disabled={pending}>Refresh delivery status</button>
<small>Delivery retries survive restarts. Changing the server, topic, or enabled setting starts with future alerts. A lost response can cause a duplicate delivery. Database backups include the saved token; configuration exports exclude it.</small>
{#if deliveries.length}<ul>{#each deliveries.slice(0,8) as item}<li>{item.title} — {item.state}{#if item.error}: {item.error}{/if}</li>{/each}</ul>{/if}
{#if error}<p class="error" role="alert">{error}</p>{/if}{#if notice}<p class="notice" role="status">{notice}</p>{/if}</section>
