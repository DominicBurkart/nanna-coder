{ flake ? ../flake.nix, required ? [ "/evals/scorecards/" ] }:
let
  text = builtins.readFile flake;
  parts = builtins.tail (builtins.filter builtins.isString (builtins.split "cleanSourceWith" text));
  body = p: builtins.head (builtins.filter builtins.isString (builtins.split "\n *};" p));
  captures = re: s: map builtins.head (builtins.filter builtins.isList (builtins.split re s));
  infixes = b: captures ''hasInfix "([^"]*)" path'' b;
  blocks = map body parts;
  keeps = b: needle: builtins.elem needle (infixes b);
in
assert builtins.length blocks >= 1;
assert builtins.all (b: builtins.all (keeps b) required) blocks;
true
