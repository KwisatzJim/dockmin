<script>
 import {tick,onDestroy} from 'svelte';
 import '@xterm/xterm/css/xterm.css';
 export let target,api,close;
 let shell='/bin/sh',confirmation='',phase='confirm',pending=false,error='',state='',node,terminal,fit,connection,observer;
 function dialog(node){node.showModal();return{destroy(){node.close();}};}
 function disconnect(){observer?.disconnect();connection?.close();connection=null;terminal?.dispose();terminal=null;}
 async function connect(event){event?.preventDefault();pending=true;error='';disconnect();try{
  const result=await api(`/hosts/${target.hostId}/containers/${target.containerId}/terminal-ticket`,{method:'POST',body:JSON.stringify({shell,confirm:confirmation})});
  phase='live';await tick();const[{Terminal},{FitAddon}]=await Promise.all([import('@xterm/xterm'),import('@xterm/addon-fit')]);
  terminal=new Terminal({cursorBlink:true,fontSize:13,theme:{background:'#0c141a',foreground:'#dce5e9'},scrollback:2000});fit=new FitAddon();terminal.loadAddon(fit);terminal.open(node);fit.fit();state='Connecting…';
  connection=new WebSocket(`${location.protocol==='https:'?'wss:':'ws:'}//${location.host}/api/hosts/${target.hostId}/containers/${target.containerId}/terminal?ticket=${encodeURIComponent(result.ticket)}`);connection.binaryType='arraybuffer';
  connection.onopen=()=>{state='Connected';fit.fit();connection.send(JSON.stringify({type:'resize',cols:Math.max(20,Math.min(400,terminal.cols)),rows:Math.max(5,Math.min(200,terminal.rows))}));terminal.focus();};
  connection.onmessage=event=>terminal?.write(typeof event.data==='string'?event.data:new Uint8Array(event.data));connection.onerror=()=>state='Connection failed';connection.onclose=()=>state='Disconnected';
  terminal.onData(data=>{if(connection?.readyState===WebSocket.OPEN)for(let i=0;i<data.length;i+=2048)connection.send(JSON.stringify({type:'input',data:data.slice(i,i+2048)}));});
  terminal.onResize(size=>{if(connection?.readyState===WebSocket.OPEN)connection.send(JSON.stringify({type:'resize',cols:Math.max(20,Math.min(400,size.cols)),rows:Math.max(5,Math.min(200,size.rows))}));});
  observer=new ResizeObserver(()=>fit?.fit());observer.observe(node);
 }catch(e){error=e.message;phase='confirm';}finally{pending=false;}}
 onDestroy(disconnect);
</script>
<dialog class="modal wide-modal" use:dialog aria-label={`Container terminal · ${target.name}`} oncancel={close} onclose={close}><button class="modal-close" aria-label="Close terminal" onclick={close}>×</button><h2>Terminal · {target.name}</h2>
{#if phase==='confirm'}<p>Open an interactive shell inside this running container. Commands can change its data. Input and output are not saved to operation history.</p><form onsubmit={connect}><label>Shell<select bind:value={shell}>{#each ['/bin/sh','/bin/bash','/bin/ash'] as path}<option>{path}</option>{/each}</select></label><label>Type {target.name}<input bind:value={confirmation} autocomplete="off"/></label><button class="primary" disabled={pending||confirmation!==target.name}>{pending?'Connecting…':'Open terminal'}</button></form>
{:else}<p>{state}</p><div class="terminal-body" bind:this={node}></div><small>Closing disconnects the session. Commands already executed are not undone.</small>{#if state==='Disconnected'||state==='Connection failed'}<button onclick={connect} disabled={pending}>Reconnect</button>{/if}{/if}
{#if error}<p class="error" role="alert">{error}</p>{/if}
</dialog>
