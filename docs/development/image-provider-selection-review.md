# Second image provider selection review

> Reviewed: 2026-08-12  
> Decision: OpenRouter dedicated Image API

M3-20 considered FAL and OpenRouter. Xana selects OpenRouter because its current
dedicated Image API publishes model discovery, per-endpoint capabilities and
pricing, explicit upstream provider pinning, `allow_fallbacks`, usage cost, and
cancellation/billing behavior. It is a different provider, catalog, model
namespace, routing policy, capability schema, and billing response from direct
OpenAI even though both use Bearer-authenticated JSON.

Primary sources:

- [OpenRouter image generation](https://openrouter.ai/docs/guides/overview/multimodal/image-generation)
  documents `/api/v1/images`, catalogs, capability descriptors, base64 plus
  media type, usage cost, provider pinning, fallback control, and billing.
- [Recraft V4 on OpenRouter](https://openrouter.ai/recraft/recraft-v4)
  documents the example model, Recraft provider, capability, and current price.

Xana's `openrouter.images` subset requires a namespaced model and explicit
`provider_only`. It sends `provider.only` and always sets
`allow_fallbacks = false`; upstream health cannot silently change the paid
provider. It accepts one raster image and bounded normalized options. Reference
images, SVG, streaming partials, arbitrary passthrough, and automatic catalog
selection are deferred. CI uses a fake endpoint; a real key/live call remains
owner-authorized and optional.
