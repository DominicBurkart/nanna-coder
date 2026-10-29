import subprocess,sys,re
P='harness/src/task.rs'
orig=open(P).read()
muts=[
 ("submit_spawn stores Unaudited", "Some(BoundIdentity::Audited(identity)),\n            )\n            .await)", "Some(BoundIdentity::Unaudited(identity.identity)),\n            )\n            .await)", "test_submit_spawn_binds_the_audited", "--lib"),
 ("admit_restored stores Unaudited", ".insert(queued.id, BoundIdentity::Audited(identity));", ".insert(queued.id, BoundIdentity::Unaudited(identity.identity));", "restore_binds_the_audited", "--lib"),
 ("catalog resolves as Audited? (forge)", "Some(identity) => Ok(Some(BoundIdentity::Unaudited(identity.clone()))),", "Some(identity) => Ok(Some(BoundIdentity::Audited(AuditedIdentity { identity: identity.clone() }))),", "spawn_gate_structural", "--test"),
 ("second constructor", "impl AuditedIdentity {\n", "impl AuditedIdentity {\n    #[allow(dead_code)]\n    pub(crate) fn forge(identity: AgentIdentity) -> Self {\n        Self { identity }\n    }\n\n", "spawn_gate_structural", "--test"),
 ("macro-hidden variant", "pub(crate) fn identity(&self) -> &AgentIdentity {", "#[allow(dead_code)]\n    pub(crate) fn sneak(token: AuditedIdentity) -> Vec<BoundIdentity> {\n        vec![BoundIdentity::Audited(token)]\n    }\n\n    pub(crate) fn identity(&self) -> &AgentIdentity {", "spawn_gate_structural", "--test"),
 ("unaudited drops identity in into_identity", "BoundIdentity::Unaudited(identity) => identity,\n        }\n    }\n}", "BoundIdentity::Unaudited(identity) => identity,\n        }\n    }\n}", "none", "--lib"),
]
for name,old,new,flt,kind in [m for m in muts if m[0] in ('submit_spawn stores Unaudited','macro-hidden variant')]:
    if name.startswith("unaudited drops"): continue
    if orig.count(old)<1: print(name,"NOT APPLIED (pattern missing)"); continue
    open(P,'w').write(orig.replace(old,new,1))
    cmd=["cargo","test","-p","harness"]+([ "--lib",flt] if kind=="--lib" else ["--test",flt])
    r=subprocess.run(cmd,capture_output=True,text=True)
    out=re.sub(r'\x1b\[[0-9;]*m','',r.stdout+r.stderr)
    if "error[" in out or "could not compile" in out: res="COMPILE ERROR"
    elif r.returncode!=0 and "FAILED" in out or "failed" in out and r.returncode!=0: res="KILLED"
    else: res="SURVIVED"
    print(name,res,flush=True)
open(P,'w').write(orig)
