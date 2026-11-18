# Programmable self-assembly in a thousand-robot swarm

A thousand Kilobots assemble into shapes given only as a picture and four environmental
landmarks. No robot knows the target shape or its own place in it.

- The shape is compiled offline into edge-following gradients.
- Robots move along gradient contours; collisions and noise are what makes it work.
- Assembly tolerates robots that fail or are added mid-run.

Worth comparing with Reynolds: same local-rule idea, but here the goal is a static shape
rather than a moving formation, and the rules are derived from a compiler rather than hand-set.
