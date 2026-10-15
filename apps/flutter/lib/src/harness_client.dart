/// Client for the P4b harness dev endpoint (stands in for NestJS P5):
/// `GET /state` → `{"port": int, "grants": {user: jwt}, "pairs": [...]}`.
library;

import 'dart:convert';
import 'dart:io';

/// One pairwise payload key (plan §3.2): AES-128-GCM key shared by exactly
/// one user pair, with its room-wide unique `kid`.
class PairKey {
  const PairKey({
    required this.a,
    required this.b,
    required this.kid,
    required this.key,
  });

  final String a;
  final String b;
  final int kid;
  final List<int> key;

  /// The pair as seen from [user]'s side (the key applies to the peer).
  bool involves(String user) => a == user || b == user;

  String peerOf(String user) => a == user ? b : a;
}

/// Harness state used to bootstrap a demo call.
class HarnessState {
  const HarnessState({
    required this.port,
    required this.grants,
    required this.pairs,
  });

  final int port;
  final Map<String, String> grants;
  final List<PairKey> pairs;

  /// Media address (`host:port`) for the harness server.
  String mediaAddress(String host) => '$host:$port';

  /// The pairwise key [user] uses to seal toward / open from everyone else.
  PairKey? pairFor(String user) {
    for (final p in pairs) {
      if (p.involves(user)) return p;
    }
    return null;
  }

  /// Parse the harness `/state` JSON.
  static HarnessState parse(String json) {
    final Map<String, dynamic> state = jsonDecode(json) as Map<String, dynamic>;
    final grants = (state['grants'] as Map<String, dynamic>).cast<String, String>();
    final pairs = <PairKey>[
      for (final raw in state['pairs'] as List)
        PairKey(
          a: (raw as Map<String, dynamic>)['a']! as String,
          b: raw['b']! as String,
          kid: raw['kid']! as int,
          key: _hex(raw['key']! as String),
        ),
    ];
    return HarnessState(
      port: state['port']! as int,
      grants: grants,
      pairs: pairs,
    );
  }

  /// Fetch `GET <stateUrl>` (default `http://127.0.0.1:7860/state`).
  static Future<HarnessState> fetch(String stateUrl) async {
    final client = HttpClient();
    try {
      final req = await client.getUrl(Uri.parse(stateUrl));
      final resp = await req.close().timeout(const Duration(seconds: 5));
      final body = await resp.transform(utf8.decoder).join();
      if (resp.statusCode != HttpStatus.ok) {
        throw HttpException('GET $stateUrl → HTTP ${resp.statusCode}');
      }
      return parse(body);
    } finally {
      client.close(force: true);
    }
  }
}

List<int> _hex(String s) => [
      for (var i = 0; i < s.length; i += 2)
        int.parse(s.substring(i, i + 2), radix: 16),
    ];
