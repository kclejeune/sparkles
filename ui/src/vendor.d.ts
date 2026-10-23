declare module 'cytoscape-fcose' {
  const ext: import('cytoscape').Ext;
  export default ext;
}

/** One ForceAtlas2 iteration over the flat node and edge matrices (graphology). */
declare module 'graphology-layout-forceatlas2/iterate.js' {
  const iterate: (
    settings: Record<string, number | boolean>,
    nodes: Float32Array,
    edges: Float32Array,
  ) => void;
  export default iterate;
}
