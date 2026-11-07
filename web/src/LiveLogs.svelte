<script>
  import {onMount} from 'svelte';
  export let stack, close;
  let source, output='', state='Connecting…', follow=true;
  function disconnect(){source?.close();source=null;follow=false;}
  function connect(){disconnect();follow=true;state='Connecting…';source=new EventSource(stack.container ? `/api/hosts/${stack.host_id}/containers/${stack.id}/logs` : `/api/stacks/${stack.id}/logs`);
    source.addEventListener('log',event=>{state='Live';output=(output+event.data).slice(-131072);});
    source.addEventListener('end',event=>{state=event.data;disconnect();});
    source.onerror=()=>{state='Connection closed. Reconnect to continue.';disconnect();};
  }
  onMount(()=>{connect();return disconnect;});
</script>
<section class="output-panel" aria-label="Live logs"><div class="output-header"><div><strong>Live logs · {stack.name}</strong><span>{state}</span></div><button onclick={close} aria-label="Close live logs">×</button></div><pre>{output||'Waiting for container output…'}</pre><div class="toolbar-actions"><button onclick={()=>{state='Paused';disconnect();}} disabled={!follow}>Pause</button><button onclick={connect} disabled={follow}>Reconnect</button><button onclick={()=>output=''}>Clear</button></div><small>Last 150 lines, then live output. Closing or pausing disconnects the stream. Output may contain sensitive information.</small></section>
