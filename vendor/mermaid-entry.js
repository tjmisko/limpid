import mermaid from 'mermaid';

mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: 'neutral' });
window.limpidRenderMermaid = async (id, source) => {
  const { svg } = await mermaid.render(id, source);
  return svg;
};
