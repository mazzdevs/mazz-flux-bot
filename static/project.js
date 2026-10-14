const projectId = new URLSearchParams(location.search).get('id');
const base = `/api/v2/projects/${encodeURIComponent(projectId || '')}`;
const statuses = [['pending','Pending'],['in_progress','In Progress'],['review','Review'],['merged','Merged'],['flags_enabled','Flags Enabled'],['flags_cleaned','Flags Cleaned']];
let current, refreshing = false, activityEntries = [], pendingRefresh = false;
const el = id => document.getElementById(id);
const esc = s => String(s ?? '').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
async function api(path,method='GET',body) {
  const response=await fetch(path,{method,headers:{'Content-Type':'application/json'},...(body===undefined?{}:{body:JSON.stringify(body)})});
  const data=await response.json().catch(()=>({}));
  if(!response.ok) throw new Error(data.error?.message || data.error || `${response.status} ${response.statusText}`);
  return data;
}
function notice(message,error=false){el('notice').textContent=message;el('notice').classList.toggle('error',error);}
function field(name,label,value='',type='text') {return `<label>${label}<input name="${name}" type="${type}" value="${esc(value)}"></label>`;}
function area(name,label,value=''){return `<label>${label}<textarea name="${name}">${esc(value)}</textarea></label>`;}
function options(values,value){return values.map(([key,label])=>`<option value="${key}" ${value===key?'selected':''}>${label}</option>`).join('');}
function clearDirty(form){const container=form.closest('.editor');delete form.dataset.dirty;if(!container.querySelector('form[data-dirty=true]'))delete container.dataset.dirty;}
function nodeKey(node){
  if(node.nodeType!==Node.ELEMENT_NODE)return null;
  if(node.dataset.key)return node.dataset.key;
  if(node.id)return node.id;
  if(node.matches('form[data-form]'))return `form:${node.dataset.form}:${node.dataset.kind || ''}:${node.dataset.id || 'new'}`;
  if(node.hasAttribute('name'))return `${node.tagName}:${node.getAttribute('name')}`;
  return null;
}
function reconcile(parent,template){
  const used=new Set();let cursor=parent.firstChild;
  for(const fresh of [...template.childNodes]){
    const key=nodeKey(fresh);
    let old=key?[...parent.childNodes].find(n=>!used.has(n)&&nodeKey(n)===key):cursor;
    if(!old || used.has(old) || old.nodeType!==fresh.nodeType || old.nodeName!==fresh.nodeName || (!key&&nodeKey(old))){old=fresh.cloneNode(true);parent.insertBefore(old,cursor);}
    else {
      if(old!==cursor)parent.insertBefore(old,cursor);
      if(old.nodeType===Node.TEXT_NODE){if(old.textContent!==fresh.textContent)old.textContent=fresh.textContent;}
      else if(old.nodeType===Node.ELEMENT_NODE && old.dataset.dirty!=='true'){
        const preserve=new Set(old.matches('details')?['open','data-loaded']:[]);
        for(const attr of [...old.attributes])if(!preserve.has(attr.name)&&!fresh.hasAttribute(attr.name))old.removeAttribute(attr.name);
        for(const attr of [...fresh.attributes])if(!preserve.has(attr.name)&&old.getAttribute(attr.name)!==attr.value)old.setAttribute(attr.name,attr.value);
        if(!(old.matches('details[data-session]')&&old.dataset.loaded==='true') && !(old===document.activeElement&&old.matches('input,textarea,select')))reconcile(old,fresh);
        if(old.matches('input,textarea,select')){
          const focused=old===document.activeElement,start=old.selectionStart,end=old.selectionEnd;
          if(old.value!==fresh.value)old.value=fresh.value;
          if(old.matches('input'))old.checked=fresh.checked;
          if(focused&&typeof start==='number'&&typeof end==='number')try{old.setSelectionRange(start,end);}catch{}
        }
      }
    }
    used.add(old);cursor=old.nextSibling;
  }
  for(const child of [...parent.childNodes])if(!used.has(child)&&!(child.nodeType===Node.ELEMENT_NODE&&(child.dataset.dirty==='true'||child.querySelector('[data-dirty=true]'))))child.remove();
}
function editor(id,html){
  const node=el(id),template=document.createElement('template');template.innerHTML=html;reconcile(node,template.content);
  node.dataset.revision=current.revision;
  node.querySelectorAll('form').forEach(form=>{if(form.dataset.dirty!=='true')form.dataset.revision=current.revision;});
  if(!node.querySelector('form[data-dirty=true]'))delete node.dataset.dirty;
}
function acknowledge(result,expected,rebaseDirty=true){
  const revision=result.revision ?? result.project?.revision;
  if(!Number.isSafeInteger(revision))return;
  if(rebaseDirty)document.querySelectorAll('form[data-dirty=true]').forEach(form=>{if(Number(form.dataset.revision)===expected)form.dataset.revision=revision;});
  current.revision=Math.max(current.revision,revision);
}
function buttons(r){if(current.archived_at)return '';return `<div class="btn-group"><button data-instance="${r.id}" data-action="start">Start instance</button><button data-instance="${r.id}" data-action="stop">Stop instance</button><button data-instance="${r.id}" data-action="rename">Rename</button><button data-instance="${r.id}" data-action="message">Message</button>${!r.is_primary?`<button data-instance="${r.id}" data-action="promote">Make primary</button>`:''}<button data-instance="${r.id}" data-action="detach" class="btn-danger">Detach (retain instance)</button></div>`;}
function render(){
  const p=current, disabled=p.archived_at?'disabled':'';
  el('project-name').textContent=p.name;document.title=`${p.name} · mazz-flux-bot`;
  el('project-summary').textContent=`${statuses.find(s=>s[0]===p.status)?.[1] || p.status} · ${p.instances.length} instances · ${p.instances.filter(r=>String(r.live.status).toLowerCase()==='running').length} running · ${p.archived_at?'Archived':'Active'}`;
  editor('metadata',`<form data-form="metadata"><fieldset ${disabled}>${field('name','Project name',p.name)}${area('description','Description',p.description)}<label>Delivery status<select name="status">${options(statuses,p.status)}</select></label>${area('goal','Project goal',p.goal)}${area('heartbeat_prompt','Heartbeat prompt (blank clears)',p.heartbeat_prompt)}${field('constellation','Default constellation',p.constellation)}${field('heartbeat_interval_secs','Heartbeat interval (seconds)',p.automation.heartbeat_interval_secs,'number')}<button>Save project</button></fieldset></form>`);
  const next=p.automation.last_heartbeat_at?new Date(new Date(p.automation.last_heartbeat_at).getTime()+p.automation.heartbeat_interval_secs*1000).toLocaleString():'Due now';
  editor('automation',`<p><strong>${esc(p.automation.status)}</strong> · heartbeat ${p.automation.heartbeat_enabled?'enabled':'disabled'} · next ${p.automation.heartbeat_enabled?esc(next):'disabled'}</p><p>${esc(p.automation.last_note)}</p>${p.archived_at?'':`<button data-project-action="${p.automation.heartbeat_enabled?'pause':'start'}">${p.automation.heartbeat_enabled?'Pause project':'Start project'}</button> <button data-project-action="heartbeat/force">Force heartbeat (primary only)</button>`}`);
  editor('instances',p.instances.map(r=>`<article class="instance-card" data-key="instance-${r.id}"><h3>${esc(r.label)} ${r.is_primary?'<span class="status-pill status-running">Primary</span>':'<span class="status-pill">Related · manual only</span>'}</h3><p>${r.vape_instance_id?`<a target="_blank" rel="noopener" href="https://vape.stable.dexus.io/instances/${encodeURIComponent(r.vape_instance_id)}">${esc(r.vape_instance_id)}</a>`:'Pending provision'} · ${esc(r.live.status || r.live.status_source)} ${esc(r.live.error || r.provision_error || '')}</p>${buttons(r)}${!r.vape_instance_id&&!p.archived_at?`<button data-instance="${r.id}" data-action="provision">Provision / reconcile</button>`:''}<details data-key="config-${r.id}"><summary>Configuration</summary><p class="sub">Constellation/model are launch configuration; editing does not rebuild a running Flux instance.</p><form data-form="instance" data-id="${r.id}"><fieldset ${disabled}>${field('label','Label',r.label)}${field('role','Role',r.role)}${field('constellation','Constellation',r.constellation)}${field('model','Model (blank uses default)',r.model)}${r.is_primary?area('goal_override','Primary goal override',r.goal_override)+area('heartbeat_prompt_override','Primary heartbeat prompt override',r.heartbeat_prompt_override):''}<button>Save configuration</button></fieldset></form></details><details data-key="session-${r.id}" data-session="${r.id}"><summary>Load live session</summary><pre>Open to fetch the latest 20 messages.</pre></details></article>`).join('') || '<p>No instances. Start project to provision a default primary, or attach an existing instance below.</p>');
  editor('add-instance',`<form data-form="add-instance"><fieldset ${disabled}><label>Mode<select name="mode"><option value="attach">Attach existing</option><option value="provision">Configure new instance</option></select></label>${field('vape_instance_id','Existing VAPE instance ID')}${field('label','Label')}${field('role','Role','worker')}${field('constellation','Constellation',p.constellation)}${field('model','Model (optional)')}<label><input type="checkbox" name="is_primary"> Set primary (only when none exists)</label><button>Add relationship</button></fieldset></form>`);
  editor('flags',p.launchdarkly_flags.map(f=>resourceForm('launchdarkly-flags',f,`${field('key','Flag key',f.key)}${field('project_key','LD project',f.project_key)}${field('environment_key','Environment',f.environment_key)}<label>Fully enabled target<select name="fully_enabled_value"><option value="true" ${f.fully_enabled_value?'selected':''}>true</option><option value="false" ${!f.fully_enabled_value?'selected':''}>false</option></select></label>${field('label','Label',f.label)}${area('notes','Notes',f.notes)}<p>Current: <strong>${esc(f.status?.available?JSON.stringify(f.status.current_value):'Unavailable')}</strong> · Targeting: ${esc(f.status?.targeting_on ?? 'unknown')} · ${f.status?.matches_fully_enabled?'At target':'Not at target / unknown'}<br>${esc(f.status?.error || '')} ${esc(f.status?.fetched_at || '')}</p>`)).join('')+`<details data-key="add-flag"><summary>Track a flag</summary>${resourceForm('launchdarkly-flags',null,`${field('key','Flag key')}${field('project_key','LD project','default')}${field('environment_key','Environment','production')}<label>Fully enabled target<select name="fully_enabled_value"><option value="true">true</option><option value="false">false</option></select></label>`)}</details>`);
  editor('links',[['jira-tickets','Original Jira tickets',p.original_jira_tickets],['ec-tickets','EC tickets',p.ec_tickets],['vape-pages','VAPE pages',p.vape_pages]].map(([kind,title,entries])=>`<h3>${title}</h3>${entries.map(l=>`<a href="${esc(l.url)}" target="_blank" rel="noopener">${esc(l.label || l.key || l.url)}</a>${resourceForm(kind,l,field('url','HTTPS URL',l.url,'url')+field('key','Ticket key (optional)',l.key)+field('label','Label',l.label))}`).join('')}<details data-key="add-${kind}"><summary>Add ${title.toLowerCase()}</summary>${resourceForm(kind,null,field('url','HTTPS URL','','url')+field('key','Key (optional)')+field('label','Label'))}</details>`).join(''));
  editor('user-notes',p.additional_notes.map(n=>resourceForm('notes',n,field('title','Title (optional)',n.title)+area('content','Note',n.content)+`<p>Position ${n.position+1} <button type="button" data-note-move="${n.id}" data-step="-1">Move up</button> <button type="button" data-note-move="${n.id}" data-step="1">Move down</button></p>`)).join('')+resourceForm('notes',null,field('title','New note title (optional)')+area('content','New note')));
  const filter=el('activity-filter'), selected=filter.value;if(filter!==document.activeElement)filter.innerHTML='<option value="">All relationships</option>'+p.instances.map(r=>`<option value="${r.id}">${esc(r.label)}</option>`).join('');filter.value=selected;
  editor('lifecycle',p.archived_at?'<button data-project-action="restore">Restore (keep paused)</button> <button data-project-action="purge" class="btn-danger">Permanently purge local data</button>':'<button data-project-action="archive" class="btn-secondary">Archive project (retain instances)</button>');
}
function resourceForm(kind,item,fields){return `<form data-form="resource" data-kind="${kind}" data-id="${item?.id || ''}" class="resource-form"><fieldset ${current.archived_at?'disabled':''}>${fields}<button>${item?'Save':'Add'}</button>${item?` <button type="button" data-resource-delete="${item.id}" data-kind="${kind}" class="btn-danger">Remove</button>`:''}</fieldset></form>`;}
async function loadExtras(){
  const results=await Promise.allSettled([api(`${base}/kanban`),api(`/api/human-tasks?project_id=${encodeURIComponent(projectId)}&open=false`),api(`${base}/memory`),api(`${base}/activity-notes`),api(`/api/log?project_id=${encodeURIComponent(projectId)}&limit=100`)]);
  if(results[0].status==='fulfilled'){const tasks=results[0].value.board.tasks;editor('kanban',tasks.map(t=>`<form data-form="task" data-id="${t.id}"><fieldset ${current.archived_at?'disabled':''}>${field('title','Task',t.title)}${area('description','Details',t.description)}<label>Status<select name="status">${options([['assigned','Assigned'],['in_progress','In Progress'],['done','Done']],t.status)}</select></label><button>Save task</button> <button type="button" data-task-delete="${t.id}">Remove task</button></fieldset></form>`).join('')+`<form data-form="task"><fieldset ${current.archived_at?'disabled':''}>${field('title','New task')}${area('description','Details')}<button>Add task</button></fieldset></form>`);}
  if(results[1].status==='fulfilled')editor('human-tasks',results[1].value.entries.map(t=>`<li><div>${esc(t.description)}<p class="sub">${esc(t.status)}</p></div>${t.status==='open'&&!current.archived_at?`<button data-resolve="${t.id}">Resolve</button>`:''}</li>`).join('') || '<li>No human tasks.</li>');
  if(results[2].status==='fulfilled')el('memory').textContent=results[2].value.memory || 'No memory yet.';
  if(results[3].status==='fulfilled')el('activity-notes').innerHTML=results[3].value.notes.map(n=>`<li><small>${esc(n.created_at)}</small><pre>${esc(n.content)}</pre></li>`).join('');
  if(results[4].status==='fulfilled'){activityEntries=results[4].value.entries;renderActivity();}
}
function renderActivity(){const rid=el('activity-filter').value,relation=current.instances.find(r=>r.id===rid);el('activity').innerHTML=activityEntries.filter(e=>{if(!rid)return true;let detail={};try{detail=JSON.parse(e.detail || '{}');}catch{}return (relation?.vape_instance_id && e.instance_id===relation.vape_instance_id)||detail.relationship_id===rid;}).map(e=>`<li><strong>${esc(e.action)}</strong> · ${esc(e.instance_id || '')} · ${esc(e.created_at)}<p>${esc(e.error || e.result || '')}</p></li>`).join('');}
el('activity-filter').addEventListener('change',renderActivity);
async function refresh(){if(refreshing){pendingRefresh=true;return;}refreshing=true;try{const loaded=(await api(base)).project;if(!current||loaded.revision>=current.revision){current=loaded;render();await loadExtras();}}catch(e){notice(e.message,true);}finally{refreshing=false;if(pendingRefresh){pendingRefresh=false;await refresh();}}}
document.addEventListener('input',event=>{const editor=event.target.closest('.editor');if(editor){editor.dataset.dirty='true';const form=event.target.closest('form');if(form)form.dataset.dirty='true';}});
document.addEventListener('change',event=>{const editor=event.target.closest('.editor');if(editor){editor.dataset.dirty='true';const form=event.target.closest('form');if(form)form.dataset.dirty='true';}});
el('reload-editors').addEventListener('click',()=>{if(!confirm('Discard unsaved edits and reload?'))return;document.querySelectorAll('[data-dirty]').forEach(e=>delete e.dataset.dirty);refresh();});
document.addEventListener('submit',async event=>{
  const form=event.target.closest('form[data-form]');if(!form)return;event.preventDefault();const data=Object.fromEntries(new FormData(form));const body={expected_revision:Number(form.dataset.revision)};let path=base,method='POST';
  if(form.dataset.form==='metadata'){Object.assign(body,data,{heartbeat_interval_secs:Number(data.heartbeat_interval_secs),heartbeat_prompt:data.heartbeat_prompt || null});method='PATCH';}
  if(form.dataset.form==='add-instance'){Object.assign(body,data,{is_primary:data.is_primary==='on'});if(data.mode==='provision')delete body.vape_instance_id;if(!body.model)delete body.model;path+='/instances';}
  if(form.dataset.form==='instance'){Object.assign(body,data,{model:data.model || null});for(const key of ['goal_override','heartbeat_prompt_override'])if(key in data)body[key]=data[key] || null;path+=`/instances/${form.dataset.id}`;method='PATCH';}
  if(form.dataset.form==='resource'){Object.assign(body,data);for(const key of ['key','label','title','notes'])if(key in data && data[key]==='')body[key]=null;if('fully_enabled_value' in data)body.fully_enabled_value=data.fully_enabled_value==='true';path+=`/${form.dataset.kind}${form.dataset.id?'/'+form.dataset.id:''}`;method=form.dataset.id?'PATCH':'POST';}
  if(form.dataset.form==='task'){path+=`/kanban${form.dataset.id?'/'+form.dataset.id:''}`;Object.assign(body,data);}
  const submit=form.querySelector('button:not([type=button])');submit.disabled=true;
  try{const result=await api(path,method,body);clearDirty(form);acknowledge(result,body.expected_revision);notice('Saved.');await refresh();}catch(e){notice(`${e.message}. Unsaved values retained; reload saved values if needed.`,true);}finally{submit.disabled=false;}
});
document.addEventListener('click',async event=>{
  const button=event.target.closest('button');if(!button || !current)return;
  const revision={expected_revision:current.revision};button.disabled=true;
  const mutate=async(path,method,body,rebaseDirty=true)=>{const result=await api(path,method,body);acknowledge(result,body.expected_revision,rebaseDirty);return result;};
  try{
    if(button.dataset.projectAction){const action=button.dataset.projectAction;if(action==='purge'){const confirmation=prompt('Permanently purge local data only. Type the exact project name:');if(confirmation===null)return;await mutate(base,'DELETE',{...revision,confirmation});location.href='/';return;}
      if(action==='archive'&&!confirm('Archive project and pause automation? All instances remain untouched.'))return;await mutate(`${base}/${action}`,'POST',revision,action!=='heartbeat/force');}
    else if(button.dataset.instance){const rid=button.dataset.instance,action=button.dataset.action,path=`${base}/instances/${rid}`;
      if(action==='promote'){if(!confirm('Promote this instance? The former primary stays related. Clear former-primary prompt overrides first if set. Running automation becomes due now.'))return;await mutate(path,'PATCH',{...revision,is_primary:true});}
      else if(action==='detach'){if(!confirm('Detach relationship only? The real instance stays running.'))return;await mutate(path,'DELETE',revision);}
      else{const body={...revision};if(action==='message'){body.message=prompt('Message for this instance only:');if(!body.message)return;}if(action==='rename'){body.name=prompt('New Flux instance name:');if(!body.name)return;}if(action==='stop'&&!confirm('Stop this exact Flux instance?'))return;await mutate(`${path}/${action}`,'POST',body);}}
    else if(button.dataset.resourceDelete){if(!confirm('Remove this record?'))return;await mutate(`${base}/${button.dataset.kind}/${button.dataset.resourceDelete}`,'DELETE',revision);const form=button.closest('form');clearDirty(form);form.remove();}
    else if(button.dataset.noteMove){const ids=current.additional_notes.map(n=>n.id);const from=ids.indexOf(button.dataset.noteMove),to=Math.max(0,Math.min(ids.length-1,from+Number(button.dataset.step)));ids.splice(to,0,ids.splice(from,1)[0]);await mutate(`${base}/notes/reorder`,'POST',{...revision,ids});}
    else if(button.dataset.resolve)await mutate(`/api/human-tasks/${button.dataset.resolve}/resolve`,'POST',{});
    else if(button.dataset.taskDelete){if(!confirm('Remove task?'))return;await mutate(`${base}/kanban/${button.dataset.taskDelete}`,'DELETE',revision);const form=button.closest('form');clearDirty(form);form.remove();}
    else return;
    notice('Updated.');await refresh();
  }catch(e){notice(e.message,true);}finally{button.disabled=false;}
});
document.addEventListener('toggle',async event=>{const details=event.target;if(!details.matches?.('details[data-session]')||!details.open||details.dataset.loaded)return;const pre=details.querySelector('pre');try{const [status,session]=await Promise.all([api(`${base}/instances/${details.dataset.session}/status`),api(`${base}/instances/${details.dataset.session}/session`)]);pre.textContent=JSON.stringify({status,...session},null,2);details.dataset.loaded='true';}catch(e){pre.textContent=e.message;}},true);
if(!projectId)notice('No project ID in URL.',true);else{refresh();setInterval(refresh,15000);}
