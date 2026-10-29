import re
import subprocess
import sys

TASK = "harness/src/task.rs"
ALIAS_FN = "    #[allow(dead_code)]\n    pub(crate) fn via_{n}(a: Allowed) -> BoundIdentity {{\n        BoundIdentity::Unaudited({call}(a).1.identity)\n    }}\n\n"
IDENTITY_FN = "    pub(crate) fn identity(&self) -> &AgentIdentity {"

MUTANTS = [
    ("submit_spawn stores Unaudited",
     "Some(BoundIdentity::Audited(identity)),\n            )\n            .await)",
     "Some(BoundIdentity::Unaudited(identity.identity)),\n            )\n            .await)",
     ["--lib", "test_submit_spawn_binds_the_audited"]),
    ("admit_restored stores Unaudited",
     ".insert(queued.id, BoundIdentity::Audited(identity));",
     ".insert(queued.id, BoundIdentity::Unaudited(identity.identity));",
     ["--lib", "restore_binds_the_audited"]),
    ("catalog resolution mints Audited",
     "Some(identity) => Ok(Some(BoundIdentity::Unaudited(identity.clone()))),",
     "Some(identity) => Ok(Some(BoundIdentity::Audited(AuditedIdentity { identity: identity.clone() }))),",
     ["--test", "spawn_gate_structural"]),
    ("second AuditedIdentity constructor",
     "impl AuditedIdentity {\n",
     "impl AuditedIdentity {\n    #[allow(dead_code)]\n    pub(crate) fn forge(identity: AgentIdentity) -> Self {\n        Self { identity }\n    }\n\n",
     ["--test", "spawn_gate_structural"]),
    ("macro-hidden BoundIdentity::Audited",
     IDENTITY_FN,
     "    #[allow(dead_code)]\n    pub(crate) fn sneak(token: AuditedIdentity) -> Vec<BoundIdentity> {\n        vec![BoundIdentity::Audited(token)]\n    }\n\n" + IDENTITY_FN,
     ["--test", "spawn_gate_structural"]),
    ("type alias then Alias::split",
     IDENTITY_FN,
     ALIAS_FN.format(n="alias", call="Alias::split") + IDENTITY_FN + "\n    #[allow(dead_code)]\n    const _ALIAS: Option<Alias> = None;",
     ["--test", "spawn_gate_structural"]),
    ("use AuditedIdentity as AI then AI::split",
     IDENTITY_FN,
     ALIAS_FN.format(n="rename", call="AI::split") + IDENTITY_FN,
     ["--test", "spawn_gate_structural"]),
    ("Self::split in a second token method",
     "impl AuditedIdentity {\n",
     "impl AuditedIdentity {\n    #[allow(dead_code)]\n    fn again(allowed: Allowed) -> Self {\n        Self::split(allowed).1\n    }\n\n",
     ["--test", "spawn_gate_structural"]),
    ("AuditedIdentity derives Clone",
     "#[derive(Debug)]\npub(crate) struct AuditedIdentity {",
     "#[derive(Debug, Clone)]\npub(crate) struct AuditedIdentity {",
     ["--test", "spawn_gate_structural"]),
    ("split becomes pub(crate)",
     "    fn split(allowed: Allowed)",
     "    pub(crate) fn split(allowed: Allowed)",
     ["--test", "spawn_gate_structural"]),
    ("manual Clone impl for the token",
     "impl AuditedIdentity {\n",
     "impl Clone for AuditedIdentity {\n    fn clone(&self) -> Self {\n        Self { identity: self.identity.clone() }\n    }\n}\n\nimpl AuditedIdentity {\n",
     ["--test", "spawn_gate_structural"]),
]

EXTRA_ITEMS = {
    "type alias then Alias::split": "#[allow(dead_code)]\ntype Alias = AuditedIdentity;\n\n",
    "use AuditedIdentity as AI then AI::split": "#[allow(unused_imports)]\nuse self::AuditedIdentity as AI;\n\n",
}


def run(args):
    result = subprocess.run(["cargo", "test", "-p", "harness"] + args, capture_output=True, text=True)
    out = re.sub(r"\x1b\[[0-9;]*m", "", result.stdout + result.stderr)
    return result.returncode, out


def main():
    original = open(TASK).read()
    lines = []
    try:
        for name, old, new, args in MUTANTS:
            if original.count(old) != 1:
                lines.append(f"{name}: NOT APPLIED")
                continue
            mutated = original.replace(old, new, 1)
            item = EXTRA_ITEMS.get(name)
            if item:
                anchor = "#[derive(Debug)]\npub(crate) struct AuditedIdentity {"
                assert mutated.count(anchor) == 1
                mutated = mutated.replace(anchor, item + anchor, 1)
            open(TASK, "w").write(mutated)
            code, out = run(args)
            if "error[" in out or "could not compile" in out:
                verdict = "COMPILE ERROR"
            elif code != 0:
                verdict = "KILLED"
            else:
                verdict = "SURVIVED"
            lines.append(f"{name}: {verdict}")
            print(lines[-1], flush=True)
    finally:
        open(TASK, "w").write(original)
    open("docs/pi-mutation/audited-identity-results.txt", "w").write("\n".join(lines) + "\n")


main()
