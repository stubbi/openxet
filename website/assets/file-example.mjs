export function describeUpload(storedParts, nextParts) {
  const stored = new Set(storedParts);
  const parts = nextParts.map(label => ({ label, reused: stored.has(label) }));
  const reusedCount = parts.filter(part => part.reused).length;
  const uploadParts = parts.filter(part => !part.reused).map(part => part.label);
  return { parts, uploadParts, reusedCount, newCount: uploadParts.length };
}
