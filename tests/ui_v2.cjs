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
    const project={id:'project-test',revision:1,schema_version:2,name:'UI verification',description:'Project context',goal:'Ship safely',heartbeat_prompt:null,constellation:'empty',status:'pending',archived_at:null,created_at:'2026-01-01',updated_at:'2026-01-01',original_jira_tickets:[],ec_tickets:[],vape_pages:[],launchdarkly_flags:[],additional_notes:[],primary_instance_relationship_id:'primary',automation:{status:'paused',heartbeat_enabled:false,heartbeat_interval_secs:900,last_note:'Paused for review',last_heartbeat_at:null},instances:[{id:'primary',project_id:'project-test',vape_instance_id:'primary-upstream',label:'Implementation',role:'worker',constellation:'empty',model:null,is_primary:true,goal_override:null,heartbeat_prompt_override:null,live:{status:'running',status_source:'live'}},{id:'related',project_id:'project-test',vape_instance_id:'related-upstream',label:'Review',role:'reviewer',constellation:'empty',model:null,is_primary:false,live:{status:'stopped',status_source:'live'}}]};
    await page.route('http://mfb.test/**',async route=>{
      const request=route.request(),url=new URL(request.url());
      const respond=(body,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(body)});
      if(url.pathname.startsWith('/api/')){
        if(request.method()!=='GET'){
          const body=request.postDataJSON();calls.push({path:url.pathname,method:request.method(),body});
          if(body.expected_revision!==project.revision)return respond({error:{code:'stale_revision',message:'Project changed since this draft'}},409);
          if(request.method()==='PATCH'&&url.pathname==='/api/v2/projects/project-test'){
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
    assert.equal(await page.locator('.instance-card').count(),2);
    await page.locator('#metadata select[name=status]').selectOption('review');
    await page.locator('#metadata button').click();
    await page.waitForFunction(()=>document.querySelector('#notice').textContent==='Saved.');assert.equal(project.status,'review');
    await page.locator('#user-notes input[name=title]').fill('Rollout');
    await page.locator('#user-notes textarea[name=content]').fill('Validate first');
    await page.locator('#user-notes button').click();
    await page.waitForFunction(()=>document.querySelectorAll('#user-notes form').length===2);assert.equal(project.additional_notes[0].content,'Validate first');
    assert.equal(calls.filter(c=>c.path.includes('/session')).length,0);
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
    assert.match(await page.locator('[data-key=instance-primary]').innerText(),/stopped/);

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
    await firstNote.locator('textarea').fill('Local draft versus external change');
    const draftRevision=await firstNote.getAttribute('data-revision');
    project.revision++;project.additional_notes[0].content='External edit';
    await page.evaluate(()=>refresh());await page.waitForTimeout(100);
    assert.equal(await firstNote.getAttribute('data-revision'),draftRevision);
    await firstNote.getByRole('button',{name:'Save',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#notice').textContent.includes('Project changed'));
    assert.equal(await firstNote.locator('textarea').inputValue(),'Local draft versus external change');
    assert.equal(project.additional_notes[0].content,'External edit');

    await page.goto('http://mfb.test/');await page.locator('a.project-link').first().waitFor();
    assert.match(await page.locator('#projects-body').innerText(),/Review/);assert.deepEqual(errors,[]);
    console.log('PASS: dashboard, local note/task revision rebasing, dirty Kanban sibling deletion, true external conflicts, open disclosures/session content/keyboard focus across refresh; no browser errors');
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exit(1);});
