# Generative Adversarial Networks

Two networks trained against each other: a generator that makes samples and a discriminator
that tells samples from real data.

- The generator never sees the data, only the discriminator's gradient.
- The paper argues the game reaches a point where the generator matches the data distribution,
  but leaves the stability of training open.
- Sampled images on MNIST, Toronto faces and CIFAR-10.

The stability note aged well: mode collapse and oscillation are the practical problems, and
the paper does not solve them.
