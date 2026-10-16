// Browser interaction fixtures remain confined to this test.
const {chromium}=require('playwright');
const fs=require('node:fs');
const path=require('node:path');
const assert=require('node:assert/strict');
(async()=>{
  const browser=await chromium.launch({headless:true});
  try {
    const page=await browser.newPage({viewport:{width:1440,height:1000}});
    const calls=[],errors=[];page.on('pageerror',e=>errors.push(e.message));page.on('dialog',dialog=>dialog.accept());
    const tasks=[{id:'task-one',title:'Implementation',description:'First task',status:'assigned'},{id:'task-two',title:'Review',description:'Second task',status:'assigned'}];
    const project={id:'project-test',revision:1,schema_version:3,execution_mode:'legacy_conductor',name:'UI verification',description:'Project context',goal:'Ship safely',heartbeat_prompt:null,constellation:'empty',status:'pending',archived_at:null,created_at:'2026-01-01',updated_at:'2026-01-01',original_jira_tickets:[],ec_tickets:[],vape_pages:[],launchdarkly_flags:[],additional_notes:[],primary_instance_relationship_id:'primary',automation:{status:'paused',heartbeat_enabled:false,heartbeat_interval_secs:900,last_note:'Paused for review',last_heartbeat_at:null},instances:[{id:'primary',project_id:'project-test',vape_instance_id:'primary-upstream',label:'Implementation',role:'worker',constellation:'empty',model:null,is_primary:true,goal_override:null,heartbeat_prompt_override:null,live:{status:'running',status_source:'live'}},{id:'related',project_id:'project-test',vape_instance_id:'related-upstream',label:'Review',role:'reviewer',constellation:'empty',model:null,is_primary:false,live:{status:'stopped',status_source:'live'}}]};
    project.launchdarkly_flags=[{id:'flag-one',key:'pos.example.rollout',project_key:'default',environment_key:'production',fully_enabled_value:true,label:null,notes:null,status:{available:true,current_value:false,targeting_on:true,matches_fully_enabled:false,fetched_at:'2026-01-01T00:00:00Z',error:null}}];
    project.original_jira_tickets=[{id:'jira-one',url:'https://example.com/POS-123',key:'POS-123',label:'Original issue'}];
    await page.route('http://mfb.test/**',async route=>{
      const request=route.request(),url=new URL(request.url());
      const respond=(body,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(body)});
      if(url.pathname.startsWith('/api/')){
        if(request.method()!=='GET'){
          const body=request.postDataJSON();calls.push({path:url.pathname,method:request.method(),body});
          if(url.pathname==='/api/v2/projects'&&request.method()==='POST'){
            assert.equal(body.goal,undefined);assert.equal(body.heartbeat_prompt,undefined);
            return respond({project});
          }
          if(body.expected_revision!==project.revision)return respond({error:{code:'stale_revision',message:'Project changed since this draft'}},409);
          if(url.pathname.endsWith('/execution/mode')){project.execution_mode=body.execution_mode;}else if(request.method()==='PATCH'&&url.pathname==='/api/v2/projects/project-test'){
            Object.assign(project,body);delete project.expected_revision;
          }else if(url.pathname==='/api/v2/projects/project-test/notes'&&request.method()==='POST'){
            project.additional_notes.push({id:`note-${project.additional_notes.length+1}`,title:body.title,content:body.content,position:project.additional_notes.length});
          }else if(url.pathname.startsWith('/api/v2/projects/project-test/notes/')&&request.method()==='PATCH'){
            const note=project.additional_notes.find(n=>url.pathname.endsWith('/'+n.id));Object.assign(note,{title:body.title,content:body.content});
          }else if(url.pathname.startsWith('/api/v2/projects/project-test/kanban/')){
            const index=tasks.findIndex(t=>url.pathname.endsWith('/'+t.id));assert.notEqual(index,-1);
            if(request.method()==='DELETE')tasks.splice(index,1);else Object.assign(tasks[index],{title:body.title,description:body.description,status:body.status});
          }else if(url.pathname.endsWith('/heartbeat/force')){
            tasks[0].status='in_progress';
          }
          project.revision++;return respond({ok:true,revision:project.revision});
        }
        if(url.pathname==='/api/v2/projects/project-test')return respond({project});
        if(url.pathname.endsWith('/execution'))return respond({execution_mode:project.execution_mode,observation:{state:'no_goal',session_id:'session-ui',fetched_at:'now'},receipts:[]});
        if(url.pathname==='/api/v2/projects')return respond({projects:[project]});
        if(url.pathname.endsWith('/kanban'))return respond({board:{tasks}});
        if(url.pathname.endsWith('/activity-notes'))return respond({notes:[]});
        if(url.pathname.endsWith('/memory'))return respond({memory:'Primary memory'});
        if(url.pathname.includes('/status'))return respond({instance:{status:'running'},agent_status:{active_harness:'pida'},pida_status:{ready:true}});
        if(url.pathname.includes('/session')){calls.push({path:url.pathname,method:'GET'});return respond({session:{messages:[{role:'assistant',content:'Retain this loaded transcript'}]}});}
        if(url.pathname==='/api/constellations')return respond({constellations:[{id:'empty',name:'Empty'}]});
        return respond({entries:[]});
      }
      const file=path.join(__dirname,'..','static',url.pathname==='/'?'index.html':url.pathname);
      await route.fulfill({status:200,contentType:file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':file.endsWith('.svg')?'image/svg+xml':'text/html',body:fs.readFileSync(file)});
    });
    await page.goto('http://mfb.test/project.html?id=project-test');
    await page.locator('#metadata input[name=name]').waitFor();
    assert.equal(await page.getByRole('tab').count(),4);
    assert.equal(await page.getByRole('tab',{name:'Overview',exact:true}).getAttribute('aria-selected'),'true');
    assert.equal(await page.locator('.project-metric').count(),4);
    assert.equal(await page.locator('#instance-summary').innerText(),'2');
    assert.equal(await page.locator('#flag-summary').innerText(),'0 / 1');
    assert.match(await page.locator('#flags').innerText(),/Not at target/);
    assert.equal(await page.locator('#links a.text-link').getAttribute('href'),'https://example.com/POS-123');
    assert.equal(await page.locator('#project-panel-instances').isVisible(),false);
    assert.equal(await page.locator('.instance-card').count(),2);

    // Tracking-only projects explain prerequisites instead of exposing actions
    // that can only fail. The shortcut works from any tab and focuses the goal.
    const originalDescription=project.description;
    project.description='Detailed project context. '.repeat(120);project.goal='  ';project.instances[0].goal_override='  ';
    await page.evaluate(()=>refresh());
    assert.equal(await page.locator('[data-project-action=start]').isDisabled(),true);
    assert.equal(await page.locator('[data-project-action="heartbeat/force"]').isDisabled(),true);
    assert.equal(await page.locator('[data-project-action=start]').getAttribute('aria-describedby'),'automation-prerequisite');
    assert.match(await page.locator('#automation-prerequisite').innerText(),/Add a goal/);
    const preview=await page.locator('#project-summary').evaluate(node=>({height:node.clientHeight,full:node.scrollHeight,line:parseFloat(getComputedStyle(node).lineHeight)}));
    assert(preview.height<=preview.line*2+1,'description preview must stay within two lines');
    assert(preview.full>preview.height,'long text should be clamped');
    assert.equal(await page.locator('#metadata textarea[name=description]').inputValue(),project.description);
    await page.screenshot({path:path.join(__dirname,'../target/project-workspace-tracking-only.png')});
    await page.getByRole('tab',{name:'Activity',exact:true}).click();
    await page.locator('[data-open-goal]').focus();
    await page.keyboard.press('Enter');
    assert.equal(await page.getByRole('tab',{name:'Overview',exact:true}).getAttribute('aria-selected'),'true');
    assert.equal(await page.locator('#automation-direction-settings').getAttribute('open'),'');
    assert.equal(await page.locator('[data-open-goal]').getAttribute('aria-expanded'),'true');
    assert.equal(await page.locator('#metadata textarea[name=goal]').evaluate(node=>document.activeElement===node),true);
    const goalTop=await page.locator('#metadata textarea[name=goal]').evaluate(node=>node.getBoundingClientRect().top);
    assert(goalTop>await page.locator('.project-tabs').evaluate(node=>node.getBoundingClientRect().bottom),'goal should clear sticky tabs');
    project.instances[0].goal_override='Primary has direction';
    await page.evaluate(()=>refresh());
    assert.equal(await page.locator('[data-project-action=start]').isDisabled(),false);
    assert.equal(await page.locator('[data-project-action="heartbeat/force"]').isDisabled(),false);
    assert.equal(await page.locator('[data-open-goal]').count(),0);
    project.goal='Ship safely';project.instances[0].goal_override=null;project.description=originalDescription;
    await page.evaluate(()=>refresh());

    // Reversible archive is visually neutral; only permanent purge is danger.
    await page.getByRole('tab',{name:'Activity',exact:true}).click();
    assert.equal(await page.locator('[data-project-action=archive]').evaluate(node=>node.classList.contains('btn-secondary')),true);
    assert.equal(await page.locator('#lifecycle-card').evaluate(node=>node.classList.contains('lifecycle-danger')),false);
    project.archived_at='2026-01-02T00:00:00Z';
    await page.evaluate(()=>refresh());
    assert.equal(await page.locator('#lifecycle-card').evaluate(node=>node.classList.contains('lifecycle-danger')),true);
    assert.equal(await page.locator('[data-project-action=purge]').evaluate(node=>node.classList.contains('btn-danger')),true);
    project.archived_at=null;
    await page.evaluate(()=>refresh());
    await page.getByRole('tab',{name:'Overview',exact:true}).click();
    await page.locator('#metadata select[name=status]').selectOption('review');
    await page.locator('#metadata button').click();
    await page.waitForFunction(()=>document.querySelector('#notice').textContent==='Saved.');assert.equal(project.status,'review');
    await page.locator('#user-notes input[name=title]').fill('Rollout');
    await page.locator('#user-notes textarea[name=content]').fill('Validate first');
    await page.locator('#user-notes button').click();
    await page.waitForFunction(()=>document.querySelectorAll('#user-notes form').length===2);assert.equal(project.additional_notes[0].content,'Validate first');
    assert.equal(calls.filter(c=>c.path.includes('/session')).length,0);
    await page.getByRole('tab',{name:'Instances',exact:true}).click();
    await page.locator('#open-add-instance').focus();
    await page.keyboard.press('Enter');
    assert.equal(await page.locator('#open-add-instance').getAttribute('aria-expanded'),'true');
    assert.equal(await page.locator('#add-instance-disclosure').getAttribute('open'),'');
    assert.equal(await page.locator('#add-instance select[name=mode]').evaluate(node=>document.activeElement===node),true);
    assert(await page.locator('#add-instance select[name=mode]').evaluate(node=>node.getBoundingClientRect().top)>await page.locator('.project-tabs').evaluate(node=>node.getBoundingClientRect().bottom));
    await page.locator('#add-instance-disclosure summary').click();
    await page.waitForFunction(()=>document.querySelector('#open-add-instance').getAttribute('aria-expanded')==='false');
    await page.locator('details[data-session=related] summary').click();
    await page.waitForFunction(()=>document.querySelector('details[data-session=related]').dataset.loaded==='true');
    const sessionText=await page.locator('details[data-session=related] pre').innerText();
    await page.locator('[data-project-action=start]').focus();
    await page.evaluate(()=>{window.focusedAction=document.activeElement;});
    await page.evaluate(()=>refresh());await page.waitForTimeout(100);
    assert.equal(await page.evaluate(()=>document.activeElement===window.focusedAction),true);
    await page.locator('[data-key=config-primary] summary').click();
    await page.locator('[data-key=config-primary] input[name=label]').focus();
    await page.evaluate(()=>{window.cleanFocusedControl=document.activeElement;});
    project.instances[0].live.status='stopped';project.instances[0].label='Updated implementation';
    await page.evaluate(()=>refresh());await page.waitForTimeout(100);
    assert.equal(await page.evaluate(()=>document.activeElement===window.cleanFocusedControl),true);
    assert.equal(await page.locator('[data-key=config-primary]').getAttribute('open'),'');
    assert.equal(await page.locator('[data-key=config-primary] input[name=label]').inputValue(),'Updated implementation');
    assert.equal(await page.locator('details[data-session=related]').getAttribute('open'),'');
    assert.equal(await page.locator('details[data-session=related] pre').innerText(),sessionText);
    assert.deepEqual(calls.filter(c=>c.path.includes('/session')).map(c=>c.path),['/api/v2/projects/project-test/instances/related/session']);
    assert.match(await page.locator('[data-key=instance-primary]').innerText(),/stopped/i);

    await page.getByRole('tab',{name:'Overview',exact:true}).click();

    // A local sibling save preserves and rebases the retained note draft.
    await page.locator('#user-notes form').nth(0).locator('textarea').fill('Unsaved existing note');
    await page.locator('#user-notes form').nth(1).locator('textarea').fill('Second note');
    await page.locator('#user-notes form').nth(1).locator('button').click();
    await page.waitForFunction(()=>document.querySelectorAll('#user-notes form').length===3);
    const firstNote=page.locator('#user-notes form[data-id=note-1]');
    assert.equal(await firstNote.locator('textarea').inputValue(),'Unsaved existing note');
    assert.equal(Number(await firstNote.getAttribute('data-revision')),project.revision);
    await firstNote.getByRole('button',{name:'Save',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#user-notes form[data-id=note-1]').dataset.dirty!=='true');
    assert.equal(project.additional_notes[0].content,'Unsaved existing note');

    // Deleting a Kanban sibling must not discard another task's draft, and
    // the acknowledged revision must make that retained draft saveable.
    await page.getByRole('tab',{name:'Work',exact:true}).click();
    const firstTask=page.locator('#kanban form[data-id=task-one]');
    await firstTask.locator('textarea').fill('Keep this Kanban draft');
    await page.locator('[data-task-delete=task-two]').click();
    await page.waitForFunction(()=>!document.querySelector('#kanban form[data-id=task-two]'));
    assert.equal(await firstTask.locator('textarea').inputValue(),'Keep this Kanban draft');
    assert.equal(await firstTask.getAttribute('data-dirty'),'true');
    assert.equal(Number(await firstTask.getAttribute('data-revision')),project.revision);
    await firstTask.locator('button').first().click();
    await page.waitForFunction(()=>document.querySelector('#kanban form[data-id=task-one]').dataset.dirty!=='true');
    assert.equal(tasks[0].description,'Keep this Kanban draft');

    // Force heartbeat may mutate the same Kanban task. Its acknowledgement
    // must not rebase a dirty task and allow stale fields to overwrite the
    // conductor update without a conflict.
    await firstTask.locator('textarea').fill('Draft while heartbeat runs');
    const taskDraftRevision=await firstTask.getAttribute('data-revision');
    await page.locator('[data-project-action="heartbeat/force"]').click();
    await page.waitForFunction(()=>document.querySelector('#notice').textContent==='Updated.');
    assert.equal(tasks[0].status,'in_progress');
    assert.equal(await firstTask.getAttribute('data-revision'),taskDraftRevision);
    assert.equal(await firstTask.locator('textarea').inputValue(),'Draft while heartbeat runs');
    await firstTask.locator('button').first().click();
    await page.waitForFunction(()=>document.querySelector('#notice').textContent.includes('Project changed'));
    assert.equal(tasks[0].status,'in_progress');
    assert.equal(tasks[0].description,'Keep this Kanban draft');

    // An external revision is not silently rebased: the original draft
    // remains visible and saving reports a real conflict.
    await page.getByRole('tab',{name:'Overview',exact:true}).click();
    await firstNote.locator('textarea').fill('Local draft versus external change');
    const draftRevision=await firstNote.getAttribute('data-revision');
    project.revision++;project.additional_notes[0].content='External edit';
    await page.evaluate(()=>refresh());await page.waitForTimeout(100);
    assert.equal(await firstNote.getAttribute('data-revision'),draftRevision);
    await firstNote.getByRole('button',{name:'Save',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#notice').textContent.includes('Project changed'));
    assert.equal(await firstNote.locator('textarea').inputValue(),'Local draft versus external change');
    assert.equal(project.additional_notes[0].content,'External edit');

    await page.getByRole('tab',{name:'Activity',exact:true}).click();
    await page.reload();
    await page.locator('#metadata input[name=name]').waitFor({state:'attached'});
    assert.equal(await page.getByRole('tab',{name:'Activity',exact:true}).getAttribute('aria-selected'),'true');
    assert.equal(await page.locator('.lifecycle-card').isVisible(),true);
    await page.getByRole('tab',{name:'Activity',exact:true}).focus();
    await page.keyboard.press('Home');
    assert.equal(await page.getByRole('tab',{name:'Overview',exact:true}).getAttribute('aria-selected'),'true');
    await page.screenshot({path:path.join(__dirname,'../target/project-workspace-overview.png')});
    await page.keyboard.press('ArrowRight');
    assert.equal(await page.getByRole('tab',{name:'Instances',exact:true}).getAttribute('aria-selected'),'true');
    await page.screenshot({path:path.join(__dirname,'../target/project-workspace-desktop.png')});
    await page.setViewportSize({width:390,height:844});
    for(const tab of ['Overview','Instances','Work','Activity']){
      await page.getByRole('tab',{name:tab,exact:true}).click();
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),true,`overflow in ${tab}`);
    }
    await page.getByRole('tab',{name:'Overview',exact:true}).click();
    await page.screenshot({path:path.join(__dirname,'../target/project-workspace-mobile.png')});
    // Goal-free bootstrap and native mode expose different execution owners.
    project.execution_mode='bootstrap_only';project.goal='';project.instances[0].goal_override=null;
    await page.evaluate(()=>refresh());
    assert.equal(await page.locator('[data-project-action=start]').isDisabled(),false);
    assert.equal(await page.locator('#metadata textarea[name=goal]').count(),0);
    await page.getByRole('tab',{name:'Work',exact:true}).click();
    await page.locator('#execution-mode select').selectOption('native_goal');
    await page.locator('#execution-mode button').click();
    await page.waitForFunction(()=>document.querySelector('#native-execution textarea[name=condition]'));
    await page.setViewportSize({width:1440,height:1000});
    assert.equal(await page.locator('#native-execution textarea[name=condition]').isVisible(),true);
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),true);
    await page.setViewportSize({width:390,height:844});
    assert.equal(await page.locator('#native-execution textarea[name=condition]').isVisible(),true);
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),true);
    assert.match(await page.locator('#native-execution').innerText(),/Clear removes the goal but does not abort/);
    await page.locator('#native-execution textarea[name=condition]').fill('Tests pass without production changes');
    await page.locator('#native-execution form button').click();
    await page.waitForFunction(()=>document.querySelector('#notice').textContent.includes('activation pending'));
    assert(calls.some(c=>c.path.endsWith('/execution/goals/start')&&c.body.condition==='Tests pass without production changes'&&c.body.expected_session_id==='session-ui'));
    // The preceding save can still be completing refresh. Wait for the
    // queued refresh to render the fixture change instead of assuming that
    // refresh() waits when another refresh is already in flight.
    project.automation.heartbeat_enabled=true;
    await page.evaluate(()=>refresh());
    await page.waitForFunction(()=>document.querySelector('[data-project-action=pause]')?.textContent==='Pause monitoring only');
    assert.equal(await page.getByRole('button',{name:'Pause monitoring only',exact:true}).count(),1);
    project.automation.heartbeat_enabled=false;
    await page.goto('http://mfb.test/');await page.locator('a.project-link').first().waitFor();
    assert.match(await page.locator('#projects-body').innerText(),/Review/);assert.deepEqual(errors,[]);
    await page.locator('#create-toggle').click();
    assert.equal(await page.locator('#goal').count(),0);
    await page.locator('#constellation').fill('empty');
    await page.locator('#create-submit').click();
    await page.waitForFunction(()=>!document.querySelector('#create-dialog').open);
    assert(calls.some(c=>c.path==='/api/v2/projects'&&c.body.goal===undefined));
    console.log('PASS: desktop/mobile execution modes and native controls, goal-free bootstrap, legacy prerequisites, goal/add-instance shortcuts, clamped description, neutral archive/danger purge, desktop/mobile tabs, optional project prompts, dashboard, local note/task revision rebasing, dirty Kanban sibling deletion, true external conflicts, open disclosures/session content/keyboard focus across refresh; no browser errors');
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exit(1);});
