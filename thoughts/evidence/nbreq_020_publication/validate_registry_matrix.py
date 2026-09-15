from pathlib import Path
import argparse,hashlib,json,re,subprocess,tomllib,zipfile
parser=argparse.ArgumentParser()
parser.add_argument('--mode',choices=['candidate','published'],required=True)
parser.add_argument('--source',required=True)
args=parser.parse_args()
root=Path('C:/User/projects/nbreq')
lab=root/'target/release-publication-20260915'
hosted=lab/('hosted-'+args.mode)
if args.mode=='candidate': hosted=hosted/'attempt-2'
run=json.loads((hosted/'run.json').read_text());jobs=json.loads((hosted/'jobs.json').read_text())
assert run['head_sha']==args.source and run['conclusion']=='success'
assert len(jobs)==8 and all(j['conclusion']=='success' for j in jobs)
artifacts=json.loads((hosted/'artifacts.json').read_text())
assert len(artifacts)==8
summaries=[];locks={};tests=0;negative=0
registry='registry+https://github.com/rust-lang/crates.io-index'
for artifact in artifacts:
    path=hosted/('artifact-'+str(artifact['id'])+'.zip')
    with zipfile.ZipFile(path) as archive:
        names=archive.namelist()
        input_name=next(n for n in names if n.endswith('inputs.json'))
        prefix=input_name[:-len('inputs.json')]
        inputs=json.loads(archive.read(input_name));result=json.loads(archive.read(prefix+'result.json'))
        steps=json.loads(archive.read(prefix+'steps.json'))
        assert inputs['mode']==args.mode and result['status']=='passed'
        assert result['steps']==19 and len(steps)==19 and all(s['passed'] for s in steps)
        assert result['registry_only']==(args.mode=='published') and result['support_from_registry']
        assert inputs['package_vcs']['git']['sha1']==(args.source if args.mode=='candidate' else 'd866179719f1cd4e2efcda7e4a533fe590ae3dd6')
        negative+=sum(s['exit_code']==101 for s in steps)
        for rel,digest in inputs['test_source_files'].items():
            blob=subprocess.check_output(['git','show',args.source+':tools/release-consumer/'+rel.replace('\\','/')],cwd=root)
            assert digest in {hashlib.sha256(blob).hexdigest(),hashlib.sha256(blob.replace(b'\n',b'\r\n')).hexdigest()}
        assert len(result['cases'])==2
        for case in result['cases']:
            packages=case['packages']
            assert packages['nbreq']['source']==(registry if args.mode=='published' else None)
            for helper,(version,digest) in inputs['helper_hashes'].items():
                assert packages[helper]==dict(version=version,source=registry)
        for name in names:
            if name.endswith('/Cargo.lock'):
                data=archive.read(name);locks[hashlib.sha256(data).hexdigest()]=data
            if name.endswith('.log'):
                tests+=sum(int(n) for n in re.findall(rb'test result: ok\. (\d+) passed',archive.read(name)))
        examples=json.loads(archive.read(prefix+'examples/results.json'))
        assert len(examples)==16 and all(e['passed'] for e in examples)
        assert all(e['exit_code']==(1 if e['label']=='response-limit-failure' else 0) for e in examples)
        summaries.append(dict(artifact=artifact['name'],sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
            platform=inputs['platform'],machine=inputs['machine'],toolchains=inputs['toolchains'],
            package_sha256=inputs['archive_sha256'],package_source=inputs['package_vcs']['git']['sha1'],cases=result['cases']))
assert tests==464 and negative==32,(tests,negative)
audit_dir=lab/(args.mode+'-lock-audits');audit_dir.mkdir(exist_ok=False)
if args.mode=='candidate':
    data=(lab/'final-root-dry-run/package/nbreq-0.2.0/Cargo.lock').read_bytes()
    locks[hashlib.sha256(data).hexdigest()]=data
audits=[]
for digest,data in locks.items():
    lock=audit_dir/(digest+'.lock');lock.write_bytes(data)
    command=[str(root/'target/tools/cargo-audit/bin/cargo-audit.exe'),'audit','--db',str(lab/'advisory-db'),
        '--no-fetch','--file',str(lock),'--json']
    checked=subprocess.run(command,cwd=root,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=180)
    (audit_dir/(digest+'.json')).write_bytes(checked.stdout)
    (audit_dir/(digest+'.stderr.log')).write_bytes(checked.stderr)
    report=json.loads(checked.stdout)
    assert checked.returncode==0 and not report['vulnerabilities']['found'] and not report['warnings'],digest
    audits.append(dict(lock_sha256=digest,exit_code=0))
database=subprocess.check_output(['git','-C',str(lab/'advisory-db'),'log','-1','--format=%H %cI'],text=True).strip()
record=dict(mode=args.mode,source_commit=args.source,run_id=run['id'],url=run['html_url'],jobs_passed=8,
    consumer_tests=tests,consumer_cases=16,negative_probes=negative,example_cases=128,
    registry_only=args.mode=='published',support_from_registry=True,artifacts=summaries,
    audits=audits,advisory_database=database,raw_directory=str(hosted))
(lab/(args.mode+'-matrix-verified.json')).write_text(json.dumps(record,indent=2))
print('Verified '+args.mode+': 8 jobs, 464 tests, 128 examples, '+str(len(audits))+' audited locks. DB '+database)
