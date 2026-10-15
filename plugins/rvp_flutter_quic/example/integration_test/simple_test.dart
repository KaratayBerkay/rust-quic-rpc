import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  test('native library loads (initRustLib)', () async {
    await initRustLib();
  });
}
