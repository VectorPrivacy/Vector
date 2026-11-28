### The Moderation console's Policies tab: automatic rules, their templates, and the preview.

## Sensitivity, how much it takes to trip a rule

policies-relaxed = Relaxed
    .desc = Two hits in one message before it says anything, with lower confidence.
policies-balanced = Balanced
    .desc = One hit is enough, at this template's own confidence.
policies-strict = Strict
    .desc = One hit is enough, with higher confidence.
policies-sensitivity = Sensitivity
policies-sensitivity-intro = Sensitivity sets how much it takes to trip a rule, and how confident the result is.
policies-sensitivity-relaxed = Relaxed: two hits in one message, with lower confidence.
policies-sensitivity-balanced = Balanced: one hit, at the template's own confidence.
policies-sensitivity-strict = Strict: one hit, with higher confidence.

## Why a policy can't be previewed yet

policies-not-ready = This policy is not ready yet.
policies-no-rules = Add at least one rule before previewing.
policies-no-domains = One of your rules has no domains in it.
policies-no-words = One of your rules has no words in it.
policies-invalid = This policy is not valid.
policies-save-failed = Couldn't save the policy

## The policy list

policies-builtin = Vector's defaults
policies-builtin-running = Watching for raid waves · running
policies-builtin-replaced = Replaced by your own version below.
policies-inspect = See the rules
# $count is how many rules the policy has.
policies-active = { $count ->
    [one] { $count } rule · active
   *[other] { $count } rules · active
}
# $count is how many rules the policy has.
policies-paused = { $count ->
    [one] { $count } rule · paused
   *[other] { $count } rules · paused
}
# $reason is why the engine refused it, from the engine in English.
policies-not-running = Not running: { $reason }
policies-not-running-invalid = Not running: invalid

## Templates and the editor

# Shown in capitals
policies-templates = Start from a template
# $example is a message the template would catch.
policies-catches = Catches: { $example }
policies-after-another = only after another rule fires
policies-no-channels = This community has no channels.
policies-loading-channels = Loading channels…
policies-words-placeholder = one word per line
policies-spoiler-placeholder = Spoiler filter

## The preview

# $reason is why the preview couldn't run.
policies-preview-failed = Not ready: { $reason }
# $count is how many members it would have flagged, $cited how many messages it cited. The <b> parts are emphasised.
policies-preview-head = Over the last 7 days this would have flagged { $count ->
    [one] <b>{ $count }</b> member
   *[other] <b>{ $count }</b> members
} and cited { $cited ->
    [one] <b>{ $cited }</b> message
   *[other] <b>{ $cited }</b> messages
}.
# $count is how many messages were checked.
policies-preview-nothing = { $count ->
    [one] Checked against { $count } message, and nothing tripped this rule. Usually good news, though it also means the preview cannot tell you whether the rule works. Try a word you know someone has used.
   *[other] Checked against { $count } messages, and nothing tripped this rule. Usually good news, though it also means the preview cannot tell you whether the rule works. Try a word you know someone has used.
}
# A trusted member the rule also caught. $name is who, $count how many of their messages.
policies-shielded-name = { $count ->
    [one] { $name } ({ $count } msg)
   *[other] { $name } ({ $count } msgs)
}
# $count is how many trusted members matched, $names who, separated by commas.
policies-shielded = { $count ->
    [one] { $count } trusted member also matched this rule and was spared only by their standing: { $names }. If this rule is meant for raiders, it is catching ordinary conversation.
   *[other] { $count } trusted members also matched this rule and were spared only by their standing: { $names }. If this rule is meant for raiders, it is catching ordinary conversation.
}
# $rules names the rules the preview couldn't check, separated by commas.
policies-unchecked = Not checked here: { $rules }
# $score is how strongly the member was flagged.
policies-suspected = { $score } · suspected
# $score is how strongly the member was flagged.
policies-provable = { $score } · provable
policies-checking = Checking real history…
policies-preview = Preview on real history
policies-enable = Enable this policy
policies-preview-first = Preview before Enabling
