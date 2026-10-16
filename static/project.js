const projectId = new URLSearchParams(location.search).get('id');
const base = `/api/v2/projects/${encodeURIComponent(projectId || '')}`;
const statuses = [['pending','Pending'],['in_progress','In Progress'],['review','Review'],['merged','Merged'],['flags_enabled','Flags Enabled'],['flags_cleaned','Flags Cleaned']];
let executionObservation = null;
let current, refreshing = false, activityEntries = [], pendingRefresh = false;
const el = id => document.getElementById(id);
const newRequestId = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2,'0')).join('');
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
const projectTabs = [...document.querySelectorAll('[data-project-tab]')];
const tabStorageKey = `mfb-project-tab:${projectId}`;
function selectProjectTab(tab, focus = false) {
  if (!projectTabs.some(button => button.dataset.projectTab === tab)) tab = 'overview';
  for (const button of projectTabs) {
    const selected = button.dataset.projectTab === tab;
    button.setAttribute('aria-selected', String(selected));
    button.tabIndex = selected ? 0 : -1;
    el(button.getAttribute('aria-controls')).hidden = !selected;
    if (selected && focus) button.focus();
  }
  localStorage.setItem(tabStorageKey, tab);
}
projectTabs.forEach(button => {
  button.addEventListener('click', () => selectProjectTab(button.dataset.projectTab));
  button.addEventListener('keydown', event => {
    const index = projectTabs.indexOf(button);
    let next;
    if (event.key === 'ArrowRight') next = (index + 1) % projectTabs.length;
    if (event.key === 'ArrowLeft') next = (index - 1 + projectTabs.length) % projectTabs.length;
    if (event.key === 'Home') next = 0;
    if (event.key === 'End') next = projectTabs.length - 1;
    if (next === undefined) return;
    event.preventDefault();
    selectProjectTab(projectTabs[next].dataset.projectTab, true);
  });
});
selectProjectTab(localStorage.getItem(tabStorageKey) || 'overview');
function openWorkspaceEditor(tab, details, field) {
  selectProjectTab(tab);
  details.open = true;
  field.focus({preventScroll: true});
  const top = window.scrollY + field.getBoundingClientRect().top - document.querySelector('.project-tabs').getBoundingClientRect().height - 16;
  window.scrollTo({top, behavior: 'auto'});
}
el('open-add-instance').addEventListener('click', () => {
  openWorkspaceEditor('instances', el('add-instance-disclosure'), el('add-instance').querySelector('[name="mode"]'));
  el('open-add-instance').setAttribute('aria-expanded', 'true');
});
document.addEventListener('click', event => {
  const button = event.target.closest('[data-open-goal]');
  if (!button) return;
  openWorkspaceEditor('overview', el('automation-direction-settings'), el('metadata').querySelector('[name="goal"]'));
  button.setAttribute('aria-expanded', 'true');
});
document.addEventListener('toggle', event => {
  if (event.target.id === 'add-instance-disclosure') el('open-add-instance').setAttribute('aria-expanded', String(event.target.open));
  if (event.target.id === 'automation-direction-settings') document.querySelectorAll('[data-open-goal]').forEach(button => button.setAttribute('aria-expanded', String(event.target.open)));
}, true);
function statusClass(status) {
  const value = String(status || '').toLowerCase();
  if (['running', 'flags_enabled', 'flags_cleaned'].includes(value)) return 'status-running';
  if (['blocked', 'starting', 'pending'].includes(value)) return 'status-blocked';
  if (['failed', 'error'].includes(value)) return 'status-error';
  if (['done', 'merged', 'review'].includes(value)) return 'status-done';
  return '';
}
function pill(text, status = text) {
  return `<span class="status-pill ${statusClass(status)}">${esc(text)}</span>`;
}
function emptyState(title, text) {
  return `<div class="workspace-empty"><strong>${esc(title)}</strong><p>${esc(text)}</p></div>`;
}
function buttons(r) {
  if (current.archived_at) return '';
  const action = (name, label, style = 'btn-secondary') => `<button type="button" class="${style}" data-instance="${r.id}" data-action="${name}">${label}</button>`;
  return `<div class="instance-actions">${r.vape_instance_id ? action('start', 'Start instance') + action('stop', 'Stop instance') + action('message', 'Message', 'btn') + action('rename', 'Rename') : action('provision', 'Provision / reconcile', 'btn')}${!r.is_primary ? action('promote', 'Make primary') : ''}${action('detach', 'Detach (retain instance)', 'btn-danger')}</div>`;
}
function renderInstance(r, disabled) {
  const health = r.live.status || (r.vape_instance_id ? 'Unavailable' : 'Pending provision');
  const error = r.live.error || r.provision_error;
  return `<article class="instance-card ${r.is_primary ? 'instance-primary' : ''}" data-key="instance-${r.id}">
    <div class="instance-heading"><div><p class="eyebrow">${r.is_primary ? 'Automated primary' : 'Related · manual only'}</p><h3>${esc(r.label)}</h3></div>${pill(health)}</div>
    <dl class="instance-facts"><div><dt>Role</dt><dd>${esc(r.role)}</dd></div><div><dt>Constellation</dt><dd>${esc(r.constellation)}</dd></div><div><dt>Model</dt><dd>${esc(r.model || 'Default model')}</dd></div></dl>
    <p class="instance-identity">${r.vape_instance_id ? `<a target="_blank" rel="noopener" href="https://vape.stable.dexus.io/instances/${encodeURIComponent(r.vape_instance_id)}">Open Flux instance ↗ <code>${esc(r.vape_instance_id)}</code></a>` : 'Not provisioned yet. No Flux resources have been created.'}</p>
    ${error ? `<p class="provider-error">${esc(error)}</p>` : ''}${buttons(r)}
    <details data-key="config-${r.id}"><summary>Configuration</summary><p class="sub">Constellation/model are launch configuration; editing does not rebuild a running Flux instance.</p><form data-form="instance" data-id="${r.id}"><fieldset ${disabled}><div class="form-grid">${field('label','Label',r.label)}${field('role','Role',r.role)}${field('constellation','Constellation',r.constellation)}${field('model','Model (blank uses default)',r.model)}</div>${r.is_primary && current.execution_mode==='legacy_conductor' ? area('goal_override','Primary goal override (optional)',r.goal_override) + area('heartbeat_prompt_override','Primary heartbeat prompt override (optional)',r.heartbeat_prompt_override) : ''}<div class="form-actions"><button class="btn">Save configuration</button></div></fieldset></form></details>
    <details data-key="session-${r.id}" data-session="${r.id}"><summary>Load live session</summary><pre>Open to fetch the latest 20 messages.</pre></details>
  </article>`;
}
function renderFlag(f) {
  const status = f.status;
  const known = status?.available && status.matches_fully_enabled !== null;
  const state = known ? (status.matches_fully_enabled ? 'At target' : 'Not at target') : 'Unavailable';
  return `<div class="tracking-card" data-key="flag-${f.id}"><div class="tracking-heading"><code>${esc(f.key)}</code>${pill(state, known ? (status.matches_fully_enabled ? 'running' : 'blocked') : '')}</div>
    <div class="flag-values"><span>Current <strong>${esc(status?.available ? JSON.stringify(status.current_value) : 'Unknown')}</strong></span><span>Target <strong>${f.fully_enabled_value}</strong></span><span>Targeting <strong>${status?.targeting_on === true ? 'On' : status?.targeting_on === false ? 'Off' : 'Unknown'}</strong></span></div>
    <p class="sub">${esc(f.project_key)} / ${esc(f.environment_key)}${status?.fetched_at ? ' · ' + esc(new Date(status.fetched_at).toLocaleTimeString()) : ''}</p>${status?.error ? `<p class="provider-error">${esc(status.error)}</p>` : ''}
    <details data-key="flag-edit-${f.id}"><summary>Edit tracking</summary>${resourceForm('launchdarkly-flags', f, `${field('key','Flag key',f.key)}<div class="form-grid">${field('project_key','LD project',f.project_key)}${field('environment_key','Environment',f.environment_key)}</div><label>Fully enabled target<select name="fully_enabled_value">${options([['true','true'],['false','false']],String(f.fully_enabled_value))}</select></label>${field('label','Label',f.label)}${area('notes','Notes',f.notes)}`)}</details></div>`;
}
function render() {
  const p = current, disabled = p.archived_at ? 'disabled' : '';
  const running = p.instances.filter(r => String(r.live.status).toLowerCase() === 'running').length;
  const atTarget = p.launchdarkly_flags.filter(f => f.status?.matches_fully_enabled === true).length;
  const primary = p.instances.find(r => r.is_primary);
  el('project-name').textContent = p.name;
  document.title = `${p.name} · mazz-flux-bot`;
  el('project-summary').textContent = p.description || (p.archived_at ? 'Archived project · restore to make changes.' : 'Your delivery context, instances, and rollout in one workspace.');
  el('delivery-summary').textContent = statuses.find(s => s[0] === p.status)?.[1] || p.status;
  el('automation-summary').textContent = p.archived_at ? 'Archived' : p.automation.status;
  el('automation-detail').textContent = `${p.execution_mode || 'legacy_conductor'} · ${p.automation.heartbeat_enabled ? 'monitoring enabled' : 'monitoring off'}`;
  el('instance-summary').textContent = String(p.instances.length);
  el('instance-detail').textContent = `${running} running · ${primary ? 'Primary selected' : 'No primary selected'}`;
  el('flag-summary').textContent = `${atTarget} / ${p.launchdarkly_flags.length}`;
  el('flag-detail').textContent = p.launchdarkly_flags.length ? 'At fully enabled target' : 'No flags tracked yet';
  const effectiveGoal = primary?.goal_override?.trim() || p.goal?.trim();
  const needsDirection = p.execution_mode==='legacy_conductor' && !effectiveGoal;
  const prerequisite = needsDirection ? 'disabled aria-describedby="automation-prerequisite"' : '';
  const directionOpen = el('automation-direction-settings')?.open || false;
  editor('project-actions', p.archived_at ? '<button class="btn-secondary" data-project-action="restore">Restore project</button>' : `<div class="automation-actions"><button class="btn" data-project-action="${p.automation.heartbeat_enabled ? 'pause' : 'start'}" ${!p.automation.heartbeat_enabled ? prerequisite : ''}>${p.automation.heartbeat_enabled ? (p.execution_mode==='native_goal' ? 'Pause monitoring only' : 'Pause project') : 'Start project'}</button><button class="btn-secondary" data-project-action="heartbeat/force" ${prerequisite}>${p.execution_mode==='legacy_conductor' ? 'Force heartbeat' : 'Refresh / bootstrap'}</button></div>${needsDirection ? `<div class="automation-prerequisite"><p id="automation-prerequisite">Tracking only. Add a goal before starting automation.</p><button class="btn-secondary" type="button" data-open-goal aria-controls="automation-direction-settings" aria-expanded="${directionOpen}">Add automation goal</button></div>` : ''}`);
  editor('metadata', `<form data-form="metadata"><fieldset ${disabled}><div class="form-grid">${field('name','Project name',p.name)}<label>Delivery status<select name="status">${options(statuses,p.status)}</select></label></div>${area('description','Description',p.description)}${p.execution_mode==='legacy_conductor' ? `<details id="automation-direction-settings" data-key="automation-settings"><summary>Legacy conductor direction &amp; defaults <span class="optional-label">Optional</span></summary><p class="field-help">Leave the goal blank for tracking-only projects. A goal is needed before automation or new instance provisioning.</p>${area('goal','Project goal (optional)',p.goal)}${area('heartbeat_prompt','Heartbeat prompt (optional; blank clears)',p.heartbeat_prompt)}<div class="form-grid">${field('constellation','Default constellation',p.constellation)}${field('heartbeat_interval_secs','Heartbeat interval (seconds)',p.automation.heartbeat_interval_secs,'number')}</div></details>` : `<div class="form-grid">${field('constellation','Default constellation',p.constellation)}${field('heartbeat_interval_secs','Monitoring interval (seconds)',p.automation.heartbeat_interval_secs,'number')}</div>`}<div class="form-actions"><span class="field-help">Delivery status is never changed by automation.</span><button class="btn">Save project</button></div></fieldset></form>`);
  editor('execution-mode', `<form data-form="execution-mode"><fieldset ${disabled}><label>Execution mode<select name="execution_mode">${options([['bootstrap_only','Context bootstrap only'],['native_goal','Flux Goal Loop'],['legacy_conductor','Legacy conductor (deprecated)']],p.execution_mode)}</select></label><p class="field-help">Changing mode pauses monitoring and never launches work. Context bootstrap summarizes live context and waits. Flux Goal Loop starts only from an explicit condition.</p><div class="form-actions"><button class="btn">Select mode</button></div></fieldset></form>`);
  const next = p.automation.last_heartbeat_at ? new Date(new Date(p.automation.last_heartbeat_at).getTime() + p.automation.heartbeat_interval_secs * 1000).toLocaleString() : 'Due now';
  editor('automation', `<div class="automation-overview">${pill(p.automation.status)}<span>Heartbeat ${p.automation.heartbeat_enabled ? 'enabled' : 'disabled'}</span><span>Next: ${p.automation.heartbeat_enabled ? esc(next) : 'Not scheduled'}</span></div><p class="automation-note">${esc(p.automation.last_note || 'No automation activity yet. This project can be used for tracking without a goal.')}</p>`);
  editor('instances', p.instances.map(r => renderInstance(r,disabled)).join('') || emptyState('No Flux instances yet', 'Attach an existing instance below, or save a goal and Start project to provision a default primary.'));
  editor('add-instance', `<form data-form="add-instance"><fieldset ${disabled}><div class="form-grid"><label>Mode<select name="mode"><option value="attach">Attach existing</option><option value="provision">Configure new instance</option></select></label>${field('vape_instance_id','Existing VAPE instance ID')}${field('label','Label')}${field('role','Role','worker')}${field('constellation','Constellation',p.constellation)}${field('model','Model (optional)')}</div><label class="checkbox-label"><input type="checkbox" name="is_primary"> Set primary (only when none exists)</label><div class="form-actions"><span class="field-help">Configuring a relationship does not provision it.</span><button class="btn">Add relationship</button></div></fieldset></form>`);
  editor('flags', (p.launchdarkly_flags.length ? p.launchdarkly_flags.map(renderFlag).join('') : emptyState('No rollout flags tracked', 'Add a flag to compare its live default with your fully enabled target.')) + `<details data-key="add-flag"><summary>Track a flag</summary>${resourceForm('launchdarkly-flags',null,`${field('key','Flag key')}<div class="form-grid">${field('project_key','LD project','default')}${field('environment_key','Environment','production')}</div><label>Fully enabled target<select name="fully_enabled_value"><option value="true">true</option><option value="false">false</option></select></label>`)}</details>`);
  editor('links', [['jira-tickets','Original Jira tickets',p.original_jira_tickets],['ec-tickets','EC tickets',p.ec_tickets],['vape-pages','VAPE pages',p.vape_pages]].map(([kind,title,entries]) => `<div class="link-group" data-key="group-${kind}"><h3>${title} <span class="count-label">${entries.length}</span></h3>${entries.map(l => `<div class="linked-record" data-key="link-${l.id}"><a class="text-link" href="${esc(l.url)}" target="_blank" rel="noopener">${esc(l.label || l.key || l.url)} ↗</a><details data-key="link-edit-${l.id}"><summary>Edit link</summary>${resourceForm(kind,l,field('url','HTTPS URL',l.url,'url') + field('key','Ticket key (optional)',l.key) + field('label','Label',l.label))}</details></div>`).join('') || '<p class="field-help">Nothing linked yet.</p>'}<details data-key="add-${kind}"><summary>Add ${title.toLowerCase()}</summary>${resourceForm(kind,null,field('url','HTTPS URL','','url') + field('key','Key (optional)') + field('label','Label'))}</details></div>`).join(''));
  editor('user-notes', p.additional_notes.map(n => resourceForm('notes',n,field('title','Title (optional)',n.title) + area('content','Note',n.content) + `<div class="note-order"><span class="field-help">Note ${n.position + 1}</span><button class="btn-secondary" type="button" data-note-move="${n.id}" data-step="-1">Move up</button><button class="btn-secondary" type="button" data-note-move="${n.id}" data-step="1">Move down</button></div>`)).join('') + resourceForm('notes',null,field('title','New note title (optional)') + area('content','New note')));
  const filter = el('activity-filter'), selected = filter.value;
  if (filter !== document.activeElement) filter.innerHTML = '<option value="">All relationships</option>' + p.instances.map(r => `<option value="${r.id}">${esc(r.label)}</option>`).join('');
  filter.value = selected;
  el('lifecycle-card').classList.toggle('lifecycle-danger', Boolean(p.archived_at));
  editor('lifecycle', p.archived_at ? '<button data-project-action="restore" class="btn-secondary">Restore (keep paused)</button> <button data-project-action="purge" class="btn-danger">Permanently purge local data</button>' : '<button data-project-action="archive" class="btn-secondary">Archive project (retain instances)</button>');
}
function resourceForm(kind,item,fields) {
  return `<form data-form="resource" data-kind="${kind}" data-id="${item?.id || ''}" class="resource-form ${item ? '' : 'resource-new'}"><fieldset ${current.archived_at ? 'disabled' : ''}>${fields}<div class="form-actions"><button class="btn">${item ? 'Save' : 'Add'}</button>${item ? `<button type="button" data-resource-delete="${item.id}" data-kind="${kind}" class="btn-danger">Remove</button>` : ''}</div></fieldset></form>`;
}
async function loadExecution(){
  if(current.execution_mode!=='native_goal'){executionObservation=null;editor('native-execution', '<p class="field-help">'+(current.execution_mode==='legacy_conductor'?'Legacy direction remains stored for recovery. Convert explicitly; no legacy goal is automatically replayed in Flux.':'Start initializes/resumes the primary, loads live project context once per session, summarizes open decisions, and waits. No completion condition is inferred.')+'</p>');return;}
  try{
    const result=await api(`${base}/execution`), observed=result.observation;
    executionObservation=observed;
    const state=observed.state || 'unknown', known=observed.session_id && ['no_goal','terminal'].includes(state);
    editor('native-execution',`<div class="native-observation"><h3>Flux Goal Loop · ${esc(state)}</h3><p>Session: <code>${esc(observed.session_id || 'Unknown')}</code> · ${esc(observed.fetched_at)}</p><p>${esc(observed.condition || observed.error || 'No live condition reported.')}</p><p>${esc(observed.verdict || '')} ${esc(observed.terminal_reason || '')}</p></div><p class="field-help">Flux is the sole continuation owner. Monitoring pause does not pause Flux. Clear removes the goal but does not abort an in-flight turn. Missing state is Unknown, not success.</p><form data-form="native-goal"><fieldset ${current.archived_at?'disabled':''}>${area('condition','Exact goal condition (1–4000 UTF-16 units; no inferred project tasks)')}<div class="form-actions"><button class="btn" ${known?'':'disabled'}>Start Flux goal</button><span class="field-help">Acceptance is pending until native activation is observed.</span></div></fieldset></form><div class="instance-actions"><button type="button" class="btn-secondary" data-native-action="resume" ${state==='paused'?'':'disabled'}>Resume observed goal</button><button type="button" class="btn-danger" data-native-action="clear" ${['active','evaluating','paused'].includes(state)?'':'disabled'}>Clear goal (not abort)</button></div><details data-key="execution-receipts"><summary>Execution receipts (${result.receipts.length})</summary>${result.receipts.slice(-10).reverse().map(r=>`<article><strong>${esc(r.request.action)} · ${esc(r.progress?.dispatch_status || 'dispatch_unknown')}</strong><p>${esc(r.request.created_at)} · ${esc(r.request.session_id)}</p><pre>${esc(r.request.condition)}</pre><p>${esc(r.progress?.observation?.state || 'unknown')} · ${esc(r.progress?.observation?.verdict || '')} · ${esc(r.progress?.observation?.terminal_reason || '')}</p><code>${esc(r.request.request_id)}</code></article>`).join('')}</details>`);
  }catch(error){executionObservation=null;editor('native-execution', `<p class="provider-error">Native state unknown: ${esc(error.message)}. No legacy fallback or automatic replay.</p>`);}
}
async function loadExtras(){
  await loadExecution();
  const results=await Promise.allSettled([api(`${base}/kanban`),api(`/api/human-tasks?project_id=${encodeURIComponent(projectId)}&open=false`),api(`${base}/memory`),api(`${base}/activity-notes`),api(`/api/log?project_id=${encodeURIComponent(projectId)}&limit=100`)]);
  if(results[0].status==='fulfilled'){const tasks=results[0].value.board.tasks;editor('kanban',tasks.map(t=>`<form data-form="task" data-id="${t.id}"><fieldset ${current.archived_at?'disabled':''}>${field('title','Task',t.title)}${area('description','Details',t.description)}<label>Status<select name="status">${options([['assigned','Assigned'],['in_progress','In Progress'],['done','Done']],t.status)}</select></label><button class="btn">Save task</button> <button class="btn-danger" type="button" data-task-delete="${t.id}">Remove task</button></fieldset></form>`).join('')+`<form data-form="task"><fieldset ${current.archived_at?'disabled':''}>${field('title','New task')}${area('description','Details')}<button class="btn">Add task</button></fieldset></form>`);}
  if(results[1].status==='fulfilled')editor('human-tasks',results[1].value.entries.map(t=>`<li><div>${esc(t.description)}<p class="sub">${esc(t.status)}</p></div>${t.status==='open'&&!current.archived_at?`<button class="btn-secondary" data-resolve="${t.id}">Resolve</button>`:''}</li>`).join('') || '<li>No human tasks.</li>');
  if(results[2].status==='fulfilled')el('memory').textContent=results[2].value.memory || 'No memory yet.';
  if(results[3].status==='fulfilled')el('activity-notes').innerHTML=results[3].value.notes.map(n=>`<li><small>${esc(n.created_at)}</small><pre>${esc(n.content)}</pre></li>`).join('') || '<li class="empty">No conductor notes yet.</li>';
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
  if(form.dataset.form==='metadata'){Object.assign(body,data,{heartbeat_interval_secs:Number(data.heartbeat_interval_secs)});if('heartbeat_prompt' in data)body.heartbeat_prompt=data.heartbeat_prompt || null;method='PATCH';}
  if(form.dataset.form==='add-instance'){Object.assign(body,data,{is_primary:data.is_primary==='on'});if(data.mode==='provision')delete body.vape_instance_id;if(!body.model)delete body.model;path+='/instances';}
  if(form.dataset.form==='instance'){Object.assign(body,data,{model:data.model || null});for(const key of ['goal_override','heartbeat_prompt_override'])if(key in data)body[key]=data[key] || null;path+=`/instances/${form.dataset.id}`;method='PATCH';}
  if(form.dataset.form==='resource'){Object.assign(body,data);for(const key of ['key','label','title','notes'])if(key in data && data[key]==='')body[key]=null;if('fully_enabled_value' in data)body.fully_enabled_value=data.fully_enabled_value==='true';path+=`/${form.dataset.kind}${form.dataset.id?'/'+form.dataset.id:''}`;method=form.dataset.id?'PATCH':'POST';}
  if(form.dataset.form==='task'){path+=`/kanban${form.dataset.id?'/'+form.dataset.id:''}`;Object.assign(body,data);}
  if(form.dataset.form==='execution-mode'){path+='/execution/mode';body.execution_mode=data.execution_mode;}
  if(form.dataset.form==='native-goal'){
    if(!executionObservation?.session_id){notice('Refresh the native session before dispatch.',true);return;}
    path+='/execution/goals/start';body.condition=data.condition;body.expected_session_id=executionObservation.session_id;
    body.request_id=form.dataset.requestId || (form.dataset.requestId=newRequestId());
  }
  const submit=form.querySelector('button:not([type=button])');submit.disabled=true;
  try{const result=await api(path,method,body);clearDirty(form);if(form.dataset.form==='native-goal')delete form.dataset.requestId;acknowledge(result,body.expected_revision);notice(form.dataset.form==='native-goal'?'Request accepted; activation pending native observation.':'Saved.');await refresh();}catch(e){notice(`${e.message}. Unsaved values retained; reload saved values if needed.`,true);}finally{submit.disabled=false;}
});
document.addEventListener('click',async event=>{
  const button=event.target.closest('button');if(!button || !current)return;
  const revision={expected_revision:current.revision};button.disabled=true;
  const mutate=async(path,method,body,rebaseDirty=true)=>{const result=await api(path,method,body);acknowledge(result,body.expected_revision,rebaseDirty);return result;};
  try{
    if(button.dataset.projectAction){const action=button.dataset.projectAction;if(action==='purge'){const confirmation=prompt('Permanently purge local data only. Type the exact project name:');if(confirmation===null)return;await mutate(base,'DELETE',{...revision,confirmation});location.href='/';return;}
      if(action==='archive'&&!confirm('Archive project and pause automation? All instances remain untouched.'))return;await mutate(`${base}/${action}`,'POST',revision,action!=='heartbeat/force');}
    else if(button.dataset.nativeAction){
      const action=button.dataset.nativeAction;if(!executionObservation?.session_id)throw new Error('Native session unknown; refresh first.');
      if(action==='clear'&&!confirm('Clear the Flux goal? This does not abort an in-flight turn.'))return;
      const requestId=button.dataset.requestId || (button.dataset.requestId=newRequestId());
      await mutate(`${base}/execution/goals/${action}`,'POST',{...revision,request_id:requestId,expected_session_id:executionObservation.session_id},false);delete button.dataset.requestId;
    }
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
