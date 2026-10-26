'use strict';
// Node half of the loop's run-time egress layer (RUN-02). No dependencies. Loaded with NODE_OPTIONS=--require <this file> into every Node
// process an agent command starts. It does nothing unless LOOP_NETWORK_ALLOW_FILE names the project's network-allow.json.
// net.Socket.prototype.connect (so net.connect, net.createConnection, tls.connect, http(s), http2 and fetch), dns.lookup and dns.promises.lookup
// refuse a host the list does not allow, BEFORE any DNS or TCP, with an error (code ECONNREFUSED):
//   [egress-runtime] <host>:<port> not in the project's network list
// Same host-level rule as the Python preload (sitecustomize.py): local hosts pass unless denied; an HTTP entry allows the host on its port
// (80/443 when the entry has none); a `tcp` entry allows host:port; a deny wins; an unreadable list fails closed for non-local hosts.
(function () {
  const allowFile = process.env.LOOP_NETWORK_ALLOW_FILE;
  if (!allowFile) return;
  const fs = require('fs');
  const net = require('net');
  const dns = require('dns');
  const path = require('path');

  const localNames = ['localhost', '127.0.0.1', '::1', '0.0.0.0', ''];
  const state = { stamp: null, list: null };

  function readList() {
    try {
      const info = fs.statSync(allowFile);
      const stamp = info.mtimeMs + ':' + info.size;
      if (stamp === state.stamp) return state.list;
      const content = JSON.parse(fs.readFileSync(allowFile, 'utf8').replace(/^﻿/, ''));
      if (!content || typeof content !== 'object' || !Array.isArray(content.allow)) throw new Error('shape');
      state.stamp = stamp;
      state.list = content;
    } catch (error) {
      state.stamp = null;
      state.list = null;
    }
    return state.list;
  }

  function clean(host) {
    let text = String(host === undefined || host === null ? '' : host).trim().toLowerCase();
    if (text.startsWith('[') && text.endsWith(']')) text = text.slice(1, -1);
    if (text.includes(':')) text = text.split('%')[0];
    return text.endsWith('.') && !text.endsWith('..') ? text.slice(0, -1) : text;
  }

  function hostMatches(pattern, host) {
    if (pattern.startsWith('*.')) {
      const suffix = pattern.slice(1);
      return host.endsWith(suffix) && host.length > suffix.length && host.slice(0, -suffix.length).split('.').every(Boolean);
    }
    return pattern === host;
  }

  function isLocal(host) {
    return localNames.includes(host) || host.endsWith('.localhost');
  }

  function allowed(host, port) {
    const content = readList();
    const denies = content && Array.isArray(content.deny) ? content.deny : [];
    for (const pattern of denies) {
      if (typeof pattern === 'string' && hostMatches(pattern, host)) return false;
    }
    if (isLocal(host)) return true;
    if (!content || content.broken) return false;
    const defined = Array.isArray(content.env_names) ? content.env_names : [];
    for (const entry of content.allow) {
      if (!entry || typeof entry !== 'object' || entry.origin === 'local') continue;
      if (entry.if_missing && defined.includes(entry.if_missing)) continue;
      if (!hostMatches(String(entry.host || ''), host)) continue;
      if (port === null) return true;
      const entryPort = entry.port === undefined ? null : entry.port;
      if (Array.isArray(entry.methods) && entry.methods.includes('tcp')) {
        if (entryPort === null || entryPort === port) return true;
      } else if (entryPort === port || (entryPort === null && (port === 80 || port === 443))) {
        return true;
      }
    }
    return false;
  }

  function noteDenial(host, port) {
    try {
      const info = JSON.parse(fs.readFileSync(path.join(path.dirname(path.resolve(allowFile)), 'network-runtime.json'), 'utf8').replace(/^﻿/, ''));
      if (!info.egress_log) return;
      const line = { time: new Date().toISOString().slice(0, 19), project: info.project || '', turn: info.turn === undefined ? null : info.turn,
        layer: 'node-preload', host, port, reason: "not in the project's network list" };
      fs.appendFileSync(info.egress_log, JSON.stringify(line) + '\n');
    } catch (error) { /* best effort: the refusal never depends on the log */ }
  }

  // null = allowed, else the error to raise
  function judge(host, port) {
    const name = clean(host);
    const number = port === null || port === undefined || port === '' ? null : Number(port);
    const portValue = Number.isFinite(number) ? number : null;
    if (allowed(name, portValue)) return null;
    return refusal(name, portValue);
  }

  function refusal(host, port) {
    noteDenial(host, port);
    const error = new Error("[egress-runtime] " + (port === null ? host : host + ':' + port) + " not in the project's network list");
    error.code = 'ECONNREFUSED';
    return error;
  }

  function targetOf(args) {
    let first = args[0];
    if (Array.isArray(first)) first = first[0];  // net's own normalised [options, callback]
    if (first !== null && typeof first === 'object') {
      if (first.path) return null;  // unix socket / named pipe
      return { host: first.host || 'localhost', port: first.port };
    }
    if (typeof first === 'number' || (typeof first === 'string' && /^\d+$/.test(first))) {
      return { host: typeof args[1] === 'string' ? args[1] : 'localhost', port: Number(first) };
    }
    return null;
  }

  const originalConnect = net.Socket.prototype.connect;
  net.Socket.prototype.connect = function connect(...args) {
    const target = targetOf(args);
    if (target) {
      const error = judge(target.host, target.port);
      if (error) {
        process.nextTick(() => this.destroy(error));
        return this;
      }
    }
    return originalConnect.apply(this, args);
  };

  function keepProperties(wrapper, original) {
    for (const key of Reflect.ownKeys(original)) {
      if (!Object.prototype.hasOwnProperty.call(wrapper, key)) Object.defineProperty(wrapper, key, Object.getOwnPropertyDescriptor(original, key));
    }
    return wrapper;
  }

  const originalLookup = dns.lookup;
  dns.lookup = keepProperties(function lookup(hostname, options, callback) {
    const error = hostname ? judge(hostname, null) : null;
    if (error) {
      const done = typeof options === 'function' ? options : callback;
      if (typeof done === 'function') process.nextTick(() => done(error));
      return {};
    }
    return originalLookup.apply(this, arguments);
  }, originalLookup);

  if (dns.promises && dns.promises.lookup) {
    const originalPromiseLookup = dns.promises.lookup;
    dns.promises.lookup = function lookup(hostname, options) {
      const error = hostname ? judge(hostname, null) : null;
      return error ? Promise.reject(error) : originalPromiseLookup.apply(this, arguments);
    };
  }
})();
