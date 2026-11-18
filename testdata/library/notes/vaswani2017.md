# Attention Is All You Need

The Transformer drops recurrence and convolution entirely: every position attends to every
other, so the whole sequence is processed in parallel.

- Self-attention is the only mixing operation; a layer is attention plus a feed-forward net.
- Multi-head attention lets a layer look at several relations at once; the paper's ablations
  show that one head is worse than eight.
- Positional encodings carry order, since attention itself is permutation-invariant.
- Trained on WMT 2014 English-German and English-French with 8 GPUs for 3.5 days.

Open questions for my work: cost is quadratic in sequence length, and the paper does not say
how attention behaves when the input order is unreliable (agents joining and leaving a swarm,
for example).
