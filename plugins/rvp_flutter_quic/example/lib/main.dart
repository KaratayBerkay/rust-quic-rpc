import 'package:flutter/material.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

Future<void> main() async {
  await initRustLib();
  runApp(const RvpExampleApp());
}

class RvpExampleApp extends StatelessWidget {
  const RvpExampleApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'RVP example',
      theme: ThemeData(colorSchemeSeed: Colors.indigo, useMaterial3: true),
      home: const ConnectPage(),
    );
  }
}

class ConnectPage extends StatefulWidget {
  const ConnectPage({super.key});

  @override
  State<ConnectPage> createState() => _ConnectPageState();
}

class _ConnectPageState extends State<ConnectPage> {
  final _address = TextEditingController(text: '127.0.0.1:4433');
  final _grant = TextEditingController();
  final _events = <String>[];
  RvpSession? _session;
  String? _error;

  @override
  void dispose() {
    _session?.close();
    _address.dispose();
    _grant.dispose();
    super.dispose();
  }

  Future<void> _connect() async {
    setState(() {
      _error = null;
      _events.clear();
    });
    final raw = _address.text.trim();
    final sep = raw.lastIndexOf(':');
    if (sep <= 0 || sep == raw.length - 1) {
      setState(() => _error = 'address must be host:port');
      return;
    }
    final host = raw.substring(0, sep);
    try {
      final session = await RvpSession.connect(
        address: raw,
        serverName: host,
        grantJwt: _grant.text.trim(),
      );
      session.messages.listen((msg) {
        setState(() => _events.add(msg.toString()));
      });
      setState(() {
        _session = session;
        _events.add(
          'welcome: participant ${session.welcome.participantId}, '
          'roster ${session.welcome.roster.map((e) => e.userId).toList()}',
        );
      });
    } catch (e) {
      setState(() => _error = e.toString());
    }
  }

  Future<void> _disconnect() async {
    final session = _session;
    setState(() => _session = null);
    await session?.close();
  }

  @override
  Widget build(BuildContext context) {
    final session = _session;
    return Scaffold(
      appBar: AppBar(title: const Text('RVP QUIC example')),
      body: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            TextField(
              controller: _address,
              decoration: const InputDecoration(labelText: 'server (host:port)'),
            ),
            TextField(
              controller: _grant,
              decoration: const InputDecoration(labelText: 'grant JWT'),
            ),
            const SizedBox(height: 12),
            Row(
              children: [
                FilledButton(
                  onPressed: session == null ? _connect : null,
                  child: const Text('Connect'),
                ),
                const SizedBox(width: 12),
                OutlinedButton(
                  onPressed: session == null ? null : _disconnect,
                  child: const Text('Disconnect'),
                ),
              ],
            ),
            if (_error != null)
              Padding(
                padding: const EdgeInsets.only(top: 12),
                child: Text(_error!, style: const TextStyle(color: Colors.red)),
              ),
            const SizedBox(height: 12),
            Expanded(
              child: ListView(
                children: [_events.map((e) => Text(e)).toList(growable: false)]
                    .expand((x) => x)
                    .toList(),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
