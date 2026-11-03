/**
 * A minimal ELF64 little-endian file whose only content is a version-needs
 * section: `needs` maps a library name to the version names it requires.
 * `options`: `extendedCount` stores the section count in the first section
 * header (e_shnum 0), `shnum` overrides e_shnum, `auxOffset` overrides where
 * the first version need points to its entries, `needCount` the declared number
 * of version needs, `auxCount` the declared number of versions of the first need.
 */
export function elfWithNeeds(needs, options = {}) {
  const strings = [Buffer.alloc(1)];
  let stringSize = 1;
  const stringAt = (text) => {
    const bytes = Buffer.from(`${text}\0`, 'latin1');
    strings.push(bytes);
    const at = stringSize;
    stringSize += bytes.length;
    return at;
  };
  const records = [];
  const entries = Object.entries(needs);
  entries.forEach(([library, versions], n) => {
    const need = Buffer.alloc(16);
    need.writeUInt16LE(1, 0);
    need.writeUInt16LE(
      n === 0 ? (options.auxCount ?? versions.length) : versions.length,
      2,
    );
    need.writeUInt32LE(stringAt(library), 4);
    need.writeUInt32LE(n === 0 ? (options.auxOffset ?? 16) : 16, 8);
    const auxiliaries = versions.map((version, i) => {
      const aux = Buffer.alloc(16);
      aux.writeUInt32LE(stringAt(version), 8);
      aux.writeUInt32LE(i === versions.length - 1 ? 0 : 16, 12);
      return aux;
    });
    need.writeUInt32LE(
      n === entries.length - 1 ? 0 : 16 + 16 * versions.length,
      12,
    );
    records.push(need, ...auxiliaries);
  });
  const dynstr = Buffer.concat(strings);
  const verneed = Buffer.concat(records);
  const stringsAt = 0x40;
  const needsAt = stringsAt + dynstr.length;
  const tableAt = needsAt + verneed.length;
  const header = Buffer.alloc(0x40);
  header.writeUInt32BE(0x7f454c46, 0);
  header[4] = 2;
  header[5] = 1;
  header.writeUInt16LE(0x3e, 18); // x86-64
  header.writeBigUInt64LE(BigInt(tableAt), 0x28);
  header.writeUInt16LE(0x40, 0x3a);
  header.writeUInt16LE(options.extendedCount ? 0 : (options.shnum ?? 3), 0x3c);
  const table = Buffer.alloc(3 * 0x40);
  const put = (index, type, offset, size, link, info) => {
    const at = index * 0x40;
    table.writeUInt32LE(type, at + 4);
    table.writeBigUInt64LE(BigInt(offset), at + 0x18);
    table.writeBigUInt64LE(BigInt(size), at + 0x20);
    table.writeUInt32LE(link, at + 0x28);
    table.writeUInt32LE(info, at + 0x2c);
  };
  if (options.extendedCount) table.writeBigUInt64LE(3n, 0x20); // first header: real count
  put(
    1,
    0x6ffffffe,
    needsAt,
    verneed.length,
    2,
    options.needCount ?? entries.length,
  );
  put(2, 3, stringsAt, dynstr.length, 0, 0);
  return Buffer.concat([header, dynstr, verneed, table]);
}
