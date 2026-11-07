import {afterEach,beforeEach,expect,test,vi} from 'vitest';
import {render,screen,fireEvent,waitFor,cleanup} from '@testing-library/svelte';
import ComposeEditor from './ComposeEditor.svelte';
import Schedule from './Schedule.svelte';
import StackTools from './StackTools.svelte';
import HostTools from './HostTools.svelte';
beforeEach(()=>{
 Object.defineProperty(HTMLDialogElement.prototype,'showModal',{configurable:true,value(){this.setAttribute('open','');}});
 Object.defineProperty(HTMLDialogElement.prototype,'close',{configurable:true,value(){this.removeAttribute('open');}});
 localStorage.clear();
});
afterEach(()=>{cleanup();vi.unstubAllGlobals();});
const stack={id:'s',name:'Media',host_id:'h',directory:'/srv/media',project:'media',files:['compose.yaml']};
test('Compose edits require validation and confirmation; typing after validation invalidates the preview',async()=>{
 const file={path:'/srv/media/compose.yaml',content:'services: {}',revision:'a'.repeat(64)};
 const api=vi.fn(async path=>path.endsWith('/preview')?{valid:true,diff:'diff'}:path.endsWith('/compose')?{files:[{...file}],job:{id:'job'}}:{}),onjob=vi.fn();
 render(ComposeEditor,{stack,api,onjob});await fireEvent.click(screen.getByRole('button',{name:'Edit Compose'}));
 const editor=await screen.findByLabelText(file.path);await fireEvent.input(editor,{target:{value:'services:\n  app: {}'}});
 expect(screen.queryByRole('button',{name:'Save Compose files'})).toBeNull();
 await fireEvent.click(screen.getByRole('button',{name:'Validate and preview'}));const confirm=await screen.findByLabelText('Type Media to save');await fireEvent.input(confirm,{target:{value:'Media'}});
 await fireEvent.input(editor,{target:{value:'services: {}\n'}});expect(screen.queryByRole('button',{name:'Save Compose files'})).toBeNull();
 await fireEvent.click(screen.getByRole('button',{name:'Validate and preview'}));await screen.findByRole('button',{name:'Save Compose files'});await fireEvent.submit(editor.closest('form'));await waitFor(()=>expect(onjob).toHaveBeenCalledWith('job'));
 const writes=api.mock.calls.filter(([path,options])=>path.endsWith('/compose')&&options?.method==='POST');expect(writes).toHaveLength(1);expect(JSON.parse(writes[0][1].body).confirm).toBe('Media');
});
test('scheduled checks save the enabled interval without invoking an update action',async()=>{
 const api=vi.fn(async()=>({preferences:{enabled:true,interval_minutes:1440,stack_ids:[]}}));render(Schedule,{api,stacks:[stack]});const interval=screen.getByLabelText('Check interval (minutes)');await waitFor(()=>expect(api).toHaveBeenCalledWith('/automation'));await fireEvent.input(interval,{target:{value:'60'}});await fireEvent.submit(interval.closest('form'));
 await waitFor(()=>expect(api).toHaveBeenCalledWith('/automation',expect.objectContaining({method:'POST',body:JSON.stringify({enabled:true,interval_minutes:60,stack_ids:[]})})));expect(api.mock.calls.some(([path])=>path.includes('bulk')||path.includes('actions'))).toBe(false);
});
test('data restore confirms the targets and leaves configuration restoration opt-in',async()=>{
 const backup={id:'b',created:1,bytes:1024,mounts:1,sources:['media-data']};
 const api=vi.fn(async path=>path.endsWith('/data')?{mounts:[]}:path.endsWith('/data-backups')?{backups:[backup]}:{job:{id:'restore-job'}}),onjob=vi.fn();
 render(StackTools,{stack,api,onjob});await fireEvent.click(screen.getByRole('button',{name:'Data backups'}));await fireEvent.click(await screen.findByRole('button',{name:'Restore',exact:true}));
 expect(screen.getByText('Targets: media-data')).toBeTruthy();expect(screen.getByLabelText('Also restore the matching images and Compose configuration').checked).toBe(false);
 const confirmation=screen.getByLabelText('Type RESTORE Media');await fireEvent.input(confirmation,{target:{value:'RESTORE Media'}});await fireEvent.submit(confirmation.closest('form'));await waitFor(()=>expect(onjob).toHaveBeenCalledWith('restore-job'));
 expect(api).toHaveBeenCalledWith('/stacks/s/restore',expect.objectContaining({body:JSON.stringify({backup:'b',confirm:'RESTORE Media',configuration:false})}));
});
test('host bootstrap clears one-time credentials from the form',async()=>{
 const api=vi.fn(async path=>path.endsWith('/connection')?{connection:null}:path.endsWith('/identity')?{hostname:'192.168.1.2',port:22,user:'jim',public_keys:[]}:path.endsWith('/keys')?{keys:[{algorithm:'ssh-ed25519',fingerprint:'SHA256:test',trusted:true}],has_trusted_keys:true}:{authorized:true,docker_ready:true});
 render(HostTools,{host:{id:'h',name:'Office',alias:'office'},api,stacks:[],changed:vi.fn()});await fireEvent.click(screen.getByRole('button',{name:'SSH setup'}));const input=await screen.findByLabelText('Remote SSH account password');await fireEvent.input(input,{target:{value:'one-time-password'}});
 const confirmations=screen.getAllByLabelText('Type office');await fireEvent.input(confirmations[0],{target:{value:'office'}});await fireEvent.submit(input.closest('form'));
 await waitFor(()=>expect(api).toHaveBeenCalledWith('/hosts/h/bootstrap',expect.objectContaining({body:JSON.stringify({password:'one-time-password',private_key:'',confirm:'office'})})));expect(input.value).toBe('');
});

test('ntfy preserves an existing token without returning it and clears a replacement after save',async()=>{
 const {default:Ntfy}=await import('./Ntfy.svelte');
 const api=vi.fn(async()=>({preferences:{enabled:true,server:'https://ntfy.example',topic:'alerts',token_configured:true},deliveries:[]}));render(Ntfy,{api});
 const topic=await screen.findByLabelText('ntfy topic');await waitFor(()=>expect(topic.value).toBe('alerts'));
 await fireEvent.submit(topic.closest('form'));await waitFor(()=>expect(api).toHaveBeenCalledWith('/ntfy',expect.objectContaining({body:JSON.stringify({enabled:true,server:'https://ntfy.example',topic:'alerts',token:null})})));
 await fireEvent.click(screen.getByLabelText('Replace or remove saved access token'));const token=screen.getByLabelText('ntfy access token');await fireEvent.input(token,{target:{value:'tk_secret'}});await fireEvent.submit(token.closest('form'));
 await waitFor(()=>expect(screen.queryByLabelText('ntfy access token')).toBeNull());expect(api).toHaveBeenCalledWith('/ntfy',expect.objectContaining({body:JSON.stringify({enabled:true,server:'https://ntfy.example',topic:'alerts',token:'tk_secret'})}));
});
