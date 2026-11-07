import {afterEach,expect,test,vi} from 'vitest';
import {render,screen,fireEvent,waitFor,cleanup} from '@testing-library/svelte';
import Settings from './Settings.svelte';
import HostTools from './HostTools.svelte';
import LiveLogs from './LiveLogs.svelte';
const preferences={health_refresh_seconds:60,browser_poll_seconds:5,concurrency:3,command_timeout_seconds:1800,history_limit:100};
afterEach(()=>{cleanup();vi.unstubAllGlobals();});
test('settings save numerical values and password change updates the active session',async()=>{
  const api=vi.fn(async path=>path==='/password'?{csrf:'new-csrf'}:{}),changed=vi.fn(),passwordChanged=vi.fn();
  render(Settings,{api,csrf:'old-csrf',preferences,changed,passwordChanged});
  const interval=screen.getByLabelText('Health refresh interval (seconds, 0 to disable)');
  await fireEvent.input(interval,{target:{value:'120'}});await fireEvent.submit(interval.closest('form'));
  await waitFor(()=>expect(api).toHaveBeenCalledWith('/settings',expect.objectContaining({body:expect.stringContaining('"health_refresh_seconds":120')})));
  const current=screen.getByLabelText('Current password');await fireEvent.input(current,{target:{value:'old-password-123'}});
  await fireEvent.input(screen.getByLabelText('New password'),{target:{value:'new-password-123'}});
  await fireEvent.input(screen.getByLabelText('Repeat new password'),{target:{value:'new-password-123'}});
  await fireEvent.submit(current.closest('form'));await waitFor(()=>expect(passwordChanged).toHaveBeenCalledWith('new-csrf'));
  expect(current.value).toBe('');
});
test('host discovery selects only unregistered stacks and registers explicit selections',async()=>{
  Object.defineProperty(HTMLDialogElement.prototype,'showModal',{configurable:true,value(){this.setAttribute('open','');}});
  Object.defineProperty(HTMLDialogElement.prototype,'close',{configurable:true,value(){this.removeAttribute('open');}});
  const candidate=id=>({id:'',host_id:'h',name:id,directory:'/srv/'+id,project:id,files:['compose.yml']});
  const api=vi.fn(async path=>path.endsWith('/discover')?{stacks:[candidate('registered'),candidate('new')],standalone:['plain']}:{ }),changed=vi.fn();
  render(HostTools,{host:{id:'h',name:'Office',alias:'office'},api,stacks:[candidate('registered')],changed});
  await fireEvent.click(screen.getByRole('button',{name:'Discover stacks'}));
  const button=await screen.findByRole('button',{name:'Register selected stacks (1)'});
  await fireEvent.submit(button.closest('form'));
  await waitFor(()=>expect(api).toHaveBeenCalledWith('/stacks',expect.objectContaining({body:JSON.stringify(candidate('new'))})));
  expect(api.mock.calls.filter(([path])=>path==='/stacks')).toHaveLength(1);
});
test('live logs close the connection on pause and teardown',async()=>{
  const sources=[];
  class Source {constructor(url){this.url=url;this.handlers={};this.close=vi.fn();sources.push(this);}addEventListener(name,handler){this.handlers[name]=handler;}}
  vi.stubGlobal('EventSource',Source);
  const view=render(LiveLogs,{stack:{id:'s',name:'Media'},close:vi.fn()});
  expect(sources[0].url).toBe('/api/stacks/s/logs');
  sources[0].handlers.log({data:'container is healthy\n'});
  await screen.findByText('container is healthy');
  await fireEvent.click(screen.getByRole('button',{name:'Pause'}));expect(sources[0].close).toHaveBeenCalled();
  await fireEvent.click(screen.getByRole('button',{name:'Reconnect'}));expect(sources).toHaveLength(2);
  view.unmount();expect(sources[1].close).toHaveBeenCalled();
});

test('Dockman import previews counts and requires explicit confirmation before merging',async()=>{
  const api=vi.fn(async()=>({hosts_added:1,stacks_added:1})),changed=vi.fn();
  render(Settings,{api,csrf:'csrf',preferences,changed,passwordChanged:vi.fn()});
  const value=[{id:'office',ssh_alias:'office',label:'Office',compose_dirs:[{id:'/srv/media',path:'/srv/media',label:'Media'}]}];
  const file={name:'hosts.json',size:100,text:async()=>JSON.stringify(value)};
  await fireEvent.change(screen.getByLabelText('Import Dockmin configuration or Dockman hosts.json'),{target:{files:[file]}});
  await screen.findByText(/hosts.json: 1 hosts and 1 stacks/);
  const confirm=screen.getByLabelText('Type IMPORT CONFIGURATION to merge');
  expect(screen.getByRole('button',{name:'Import configuration',exact:true}).disabled).toBe(true);
  expect(api.mock.calls.filter(([path])=>path==='/config/import')).toHaveLength(0);
  await fireEvent.input(confirm,{target:{value:'IMPORT CONFIGURATION'}});await fireEvent.submit(confirm.closest('form'));
  await waitFor(()=>expect(api).toHaveBeenCalledWith('/config/import',expect.objectContaining({body:JSON.stringify({configuration:value,confirm:'IMPORT CONFIGURATION',apply_settings:false})})));
});

test('partially completed discovery registration can retry the remaining stacks',async()=>{
  Object.defineProperty(HTMLDialogElement.prototype,'showModal',{configurable:true,value(){this.setAttribute('open','');}});
  Object.defineProperty(HTMLDialogElement.prototype,'close',{configurable:true,value(){this.removeAttribute('open');}});
  const candidate=name=>({id:'',host_id:'h',name,directory:'/srv/'+name,project:name,files:[]});
  let failed=false;
  const api=vi.fn(async(path,options)=>{
    if(path.endsWith('/discover'))return {stacks:[candidate('one'),candidate('two')],standalone:[]};
    if(JSON.parse(options.body).name==='two' && !failed){failed=true;throw new Error('Host is busy');}
    return {};
  }),changed=vi.fn();
  render(HostTools,{host:{id:'h',name:'Office',alias:'office'},api,stacks:[],changed});
  await fireEvent.click(screen.getByRole('button',{name:'Discover stacks'}));
  const button=await screen.findByRole('button',{name:'Register selected stacks (2)'});await fireEvent.submit(button.closest('form'));
  await screen.findByRole('alert');expect(changed).toHaveBeenCalled();
  const retry=screen.getByRole('button',{name:'Register selected stacks (1)'});await fireEvent.submit(retry.closest('form'));
  await screen.findByText('Registered 1 stacks.');
  expect(api.mock.calls.filter(([path,options])=>path==='/stacks'&&JSON.parse(options.body).name==='one')).toHaveLength(1);
});
