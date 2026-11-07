<script>
  import {onMount} from 'svelte';
  import Ntfy from './Ntfy.svelte';
  export let api, stacks=[];
  let form={enabled:true,interval_minutes:1440,stack_ids:[]},pending=false,error='',notice='';
  let browserEnabled=localStorage.getItem('dockmin-browser-notifications')==='true';
  onMount(()=>{api('/automation').then(data=>{if(data.preferences)form=data.preferences;}).catch(e=>error=e.message);});
  async function save(event){event.preventDefault();pending=true;error='';try{await api('/automation',{method:'POST',body:JSON.stringify(form)});notice='Update-check schedule saved.';}catch(e){error=e.message;}finally{pending=false;}}
  async function browser(){if(!('Notification' in window)){error='This browser does not support desktop notifications.';return;}const permission=await Notification.requestPermission();browserEnabled=permission==='granted';localStorage.setItem('dockmin-browser-notifications',String(browserEnabled));if(!browserEnabled)error='Notifications were not enabled. The in-app inbox remains available.';}
</script>
<section class="settings-card"><h2>Scheduled update checks</h2><p>Check image registries automatically. Updates remain under your control. Results and failures appear in Notifications even while you’re signed out.</p>
<form onsubmit={save}><label class="inline-option"><input type="checkbox" bind:checked={form.enabled}/> Enable scheduled checks</label><label>Check interval (minutes)<input type="number" min="15" max="10080" bind:value={form.interval_minutes} required/></label>
<label>Stacks to check<select multiple bind:value={form.stack_ids} aria-label="Scheduled stacks">{#each stacks as stack}<option value={stack.id}>{stack.name}</option>{/each}</select></label><small>Leave the selection empty to check all stacks. Busy hosts are checked after their current work finishes.</small><button class="primary" disabled={pending}>Save schedule</button></form>
<button onclick={browser} disabled={browserEnabled}> {browserEnabled?'Browser notifications enabled':'Enable browser notifications'}</button>{#if browserEnabled}<button onclick={()=>{browserEnabled=false;localStorage.setItem('dockmin-browser-notifications','false');}}>Disable browser notifications</button>{/if}<small>Browser notifications work while Dockmin is open on this device. The persistent inbox records alerts at all times.</small>
{#if error}<p class="error" role="alert">{error}</p>{/if}{#if notice}<p class="notice" role="status">{notice}</p>{/if}</section>

<Ntfy {api}/>
